//! Driver tests (BUILD_WAVES.md W7), all FakeHost-scripted + a tempfile store so the tick
//! loop, effect execution, and reconciliation are exercised with zero live host.
//!
//! `driver_test` is an inline submodule of `driver.rs`, so it may reach the driver's
//! private methods (`reconcile`, `drain_outbox`, `step`, ...) directly — the tests drive
//! those units without going through the 1s `recv_timeout` loop.

use super::*;
use crate::handlers;
use crate::host::FakeHost;
use office_core::{
    AgentBinding, AgentKind, OutboundNotice, Project, ProjectConfig, ProjectPhase, Task, TaskId,
    TaskState,
};
use office_store::Store;

const WORKER_REPORT: &str = "did the work\n\nOFFICE-REPORT\nstatus: complete\nsummary: built the login form\ndelivered: /ws/deliver/login.rs\n";

fn temp_store() -> (Store, tempfile::TempDir) {
    let dir = tempfile::tempdir().expect("tempdir");
    let store = Store::open(dir.path()).expect("open store");
    (store, dir)
}

fn driver(store: Store, host: FakeHost) -> Driver<FakeHost> {
    Driver::load(store, host, "test-instance".to_string(), 4242).expect("load driver")
}

fn task(id: &str, state: TaskState) -> Task {
    Task {
        id: TaskId(id.to_string()),
        title: format!("task {id}"),
        description: "do a thing".to_string(),
        acceptance: vec!["it works".to_string()],
        blocked_by: vec![],
        priority: 0,
        state,
        bounces: 0,
        comments: vec![],
        desk: None,
        last_report: None,
        last_review: None,
        history: vec![],
    }
}

fn worker_binding(agent_id: u64, spawned_at_ms: u64) -> AgentBinding {
    AgentBinding {
        ext_agent_id: agent_id,
        session: "sess-x".to_string(),
        spawned_at_ms,
        kind: AgentKind::Worker,
    }
}

fn project(slug: &str, phase: ProjectPhase, tasks: Vec<Task>) -> Project {
    Project {
        id: office_core::ProjectId(slug.to_string()),
        name: format!("Project {slug}"),
        phase,
        prd_markdown: "# PRD\n".to_string(),
        office_transcript: vec![],
        office_summary: String::new(),
        delivery_path: Some(PathBuf::from("/ws/deliver")),
        bound_session: Some("sess-x".to_string()),
        workspace: Some(PathBuf::from("/ws")),
        epics: vec![],
        stories: vec![],
        tasks,
        config: ProjectConfig::default_config(),
        outbox: vec![],
        seq: 1,
    }
}

fn call_count(host: &FakeHost, method: &str) -> usize {
    host.calls.iter().filter(|(m, _)| m == method).count()
}

fn last_call(host: &FakeHost, method: &str) -> Option<Value> {
    host.calls
        .iter()
        .rev()
        .find(|(m, _)| m == method)
        .map(|(_, p)| p.clone())
}

// ---------------------------------------------------------------------------
// 1. one dispatch cycle
// ---------------------------------------------------------------------------

#[test]
fn tick_dispatches_ready_task_and_persists_binding() {
    let (store, _dir) = temp_store();
    let mut host = FakeHost::new();
    host.script("agents.list", json!([]));
    host.script("sessions.spawn_into", json!({ "agentId": 101, "status": "spawned" }));
    let mut d = driver(store, host);

    d.insert_for_test(project("auth", ProjectPhase::Running, vec![task("auth/t1", TaskState::Todo)]), 1_000);
    d.on_tick(1_000);

    let spawn = last_call(&d.host, "sessions.spawn_into").expect("a spawn_into call");
    assert_eq!(spawn.get("agent").and_then(Value::as_str), Some("office-worker"));
    assert_eq!(spawn.get("notify").and_then(Value::as_bool), Some(true));
    assert_eq!(spawn.get("session").and_then(Value::as_str), Some("sess-x"));
    // worker_model is None -> the model key is omitted entirely (inherit Main).
    assert!(spawn.get("model").is_none());

    // The binding, with its real id, is persisted (survives a store reload).
    let reloaded = d.store.load_project("auth").expect("reload");
    match &reloaded.tasks[0].state {
        TaskState::OnProgress { binding, .. } => assert_eq!(binding.ext_agent_id, 101),
        other => panic!("expected OnProgress with a recorded id, got {other:?}"),
    }
}

// ---------------------------------------------------------------------------
// 2. completion cycle -> review -> reviewer spawned
// ---------------------------------------------------------------------------

#[test]
fn agents_done_fetches_result_moves_to_review_and_spawns_reviewer() {
    let (store, _dir) = temp_store();
    let mut host = FakeHost::new();
    host.script("agents.result", json!({ "agentId": 101, "status": "done", "output": WORKER_REPORT }));
    host.script("sessions.spawn_into", json!({ "agentId": 202, "status": "spawned" }));
    let mut d = driver(store, host);

    let t = task(
        "auth/t1",
        TaskState::OnProgress { binding: worker_binding(101, 1_000), attempt: 1 },
    );
    d.insert_for_test(project("auth", ProjectPhase::Running, vec![t]), 1_000);

    d.handle(
        handlers::Input::Event(handlers::HostEvent::AgentsDone { agent_id: 101, status: "done".to_string() }),
        2_000,
    );

    assert_eq!(call_count(&d.host, "agents.result"), 1, "the report is fetched via agents.result");
    let reviewer = last_call(&d.host, "sessions.spawn_into").expect("reviewer spawn");
    assert_eq!(reviewer.get("agent").and_then(Value::as_str), Some("office-reviewer"));

    match &d.project("auth").unwrap().tasks[0].state {
        TaskState::Review { binding: Some(b), .. } => assert_eq!(b.ext_agent_id, 202),
        other => panic!("expected Review with a spawned reviewer, got {other:?}"),
    }
}

// ---------------------------------------------------------------------------
// 3. reconcile: killed path off error VALUES + orphan sweep
// ---------------------------------------------------------------------------

#[test]
fn reconcile_killed_path_keys_off_error_value_not_status_field() {
    let (store, _dir) = temp_store();
    let mut host = FakeHost::new();
    // The reply carries BOTH an error and a `status: running` — the driver must take the
    // killed path off the ERROR VALUE and ignore the status field (5.2.1).
    host.script("agents.status", json!({ "error": "unknown agentId: 101", "status": "running" }));
    host.script("agents.list", json!([]));
    let mut d = driver(store, host);

    // Interrupted (soft-drained) so the killed re-queue is not immediately re-dispatched.
    let t = task(
        "auth/t1",
        TaskState::OnProgress { binding: worker_binding(101, 1_000), attempt: 1 },
    );
    d.insert_for_test(project("auth", ProjectPhase::Interrupted, vec![t]), 1_000);

    d.reconcile(2_000);

    assert_eq!(call_count(&d.host, "agents.status"), 1);
    assert_eq!(
        d.project("auth").unwrap().tasks[0].state,
        TaskState::Todo,
        "an unknown-agentId reply re-queues the worker to Todo"
    );
}

#[test]
fn reconcile_session_closed_is_also_the_killed_path() {
    let (store, _dir) = temp_store();
    let mut host = FakeHost::new();
    host.script("agents.status", json!({ "error": "session closed" }));
    host.script("agents.list", json!([]));
    let mut d = driver(store, host);

    let t = task("auth/t1", TaskState::OnProgress { binding: worker_binding(101, 1_000), attempt: 1 });
    d.insert_for_test(project("auth", ProjectPhase::Interrupted, vec![t]), 1_000);

    d.reconcile(2_000);
    assert_eq!(d.project("auth").unwrap().tasks[0].state, TaskState::Todo);
}

#[test]
fn reconcile_orphan_sweep_kills_untracked_office_agent_including_gone() {
    let (store, _dir) = temp_store();
    let mut host = FakeHost::new();
    // A `status:"gone"` office-worker that no task references -> orphan-swept. `gone` is
    // consumed HERE, from agents.list (never from agents.status).
    host.script("agents.list", json!([
        { "agentId": 999, "agent": "office-worker", "status": "gone" },
        { "agentId": 12,  "agent": "general",       "status": "running" }
    ]));
    host.script("agents.kill", json!({ "killed": true }));
    let mut d = driver(store, host);

    // No live bindings -> nothing tracked -> 999 is an orphan; 12 is not ours (skip).
    d.insert_for_test(project("auth", ProjectPhase::Running, vec![task("auth/t1", TaskState::Backlog)]), 1_000);

    d.reconcile(2_000);

    assert_eq!(call_count(&d.host, "agents.kill"), 1, "exactly the orphan is killed");
    let kill = last_call(&d.host, "agents.kill").unwrap();
    assert_eq!(kill.get("agentId").and_then(Value::as_u64), Some(999));
}

// ---------------------------------------------------------------------------
// 4. runtime ceiling (5.2.4): force-kill + re-queue, no liveness poll
// ---------------------------------------------------------------------------

#[test]
fn runtime_ceiling_force_kills_over_age_binding_without_status_poll() {
    let (store, _dir) = temp_store();
    let mut host = FakeHost::new();
    host.script("agents.kill", json!({ "killed": true }));
    host.script("agents.list", json!([]));
    let mut d = driver(store, host);

    // Binding spawned at t=0; default ceiling is 20 min. now = 21 min -> expired.
    let now = 21 * 60 * 1000;
    let t = task("auth/t1", TaskState::OnProgress { binding: worker_binding(101, 0), attempt: 1 });
    // Interrupted so the kernel does not immediately re-dispatch (isolates the ceiling).
    d.insert_for_test(project("auth", ProjectPhase::Interrupted, vec![t]), 0);

    d.reconcile(now);

    assert_eq!(call_count(&d.host, "agents.kill"), 1, "the over-age binding is force-killed");
    assert_eq!(last_call(&d.host, "agents.kill").unwrap().get("agentId").and_then(Value::as_u64), Some(101));
    assert_eq!(
        call_count(&d.host, "agents.status"),
        0,
        "the ceiling fires unconditionally — no liveness poll is needed"
    );
    assert_eq!(d.project("auth").unwrap().tasks[0].state, TaskState::Todo);
}

// ---------------------------------------------------------------------------
// 5. lease-steal safety (5.6): short-circuit on {status:"sent"}, release lease
// ---------------------------------------------------------------------------

#[test]
fn cross_process_spawn_short_circuits_ready_set_and_releases_lease() {
    let (store, _dir) = temp_store();
    let mut host = FakeHost::new();
    host.script("agents.list", json!([]));
    // The bound session moved off-daemon: the FIRST spawn_into is fire-and-forget.
    host.script("sessions.spawn_into", json!({ "status": "sent", "session": "sess-x" }));
    let mut d = driver(store, host);

    // Two ready tasks: proves the rest of the set is NOT fired after the first `sent`.
    let tasks = vec![task("auth/t1", TaskState::Todo), task("auth/t2", TaskState::Todo)];
    d.insert_for_test(project("auth", ProjectPhase::Running, tasks), 1_000);
    assert!(d.holds_lease("auth"));

    d.on_tick(1_000);

    assert_eq!(
        call_count(&d.host, "sessions.spawn_into"),
        1,
        "only the first spawn is attempted; the ready set is short-circuited"
    );
    assert!(!d.holds_lease("auth"), "the lease is released rather than firing untracked duplicates");
    // Rolled back to the on-disk (pre-dispatch) state: both tasks are Todo again.
    let reloaded = d.store.load_project("auth").unwrap();
    assert!(reloaded.tasks.iter().all(|t| t.state == TaskState::Todo));
}

// ---------------------------------------------------------------------------
// 6. outbox discipline (6.5)
// ---------------------------------------------------------------------------

#[test]
fn outbox_queue_full_keeps_notice_for_retry() {
    let (store, _dir) = temp_store();
    let mut host = FakeHost::new();
    host.script("chat.prompt", json!({ "error": "prompt queue full (5)" }));
    let mut d = driver(store, host);

    let mut p = project("auth", ProjectPhase::Interrupted, vec![task("auth/t1", TaskState::Backlog)]);
    p.outbox.push(OutboundNotice { id: 1, text: "line halted".to_string(), sent: false, paused: false });
    d.insert_for_test(p, 1_000);

    d.drain_outbox(1_000);

    assert_eq!(call_count(&d.host, "chat.prompt"), 1);
    let n = &d.project("auth").unwrap().outbox[0];
    assert!(!n.sent && !n.paused, "queue-full leaves the notice unsent for the next tick");
}

#[test]
fn outbox_turn_budget_pauses_then_resumes_on_turn_end() {
    let (store, _dir) = temp_store();
    let mut host = FakeHost::new();
    host.script("chat.prompt", json!({ "error": "extension turn budget exhausted; waiting for user activity" }));
    host.script("chat.prompt", json!({ "queued": 1 }));
    let mut d = driver(store, host);

    let mut p = project("auth", ProjectPhase::Interrupted, vec![task("auth/t1", TaskState::Backlog)]);
    p.outbox.push(OutboundNotice { id: 1, text: "line halted".to_string(), sent: false, paused: false });
    d.insert_for_test(p, 1_000);

    d.drain_outbox(1_000);
    assert!(d.project("auth").unwrap().outbox[0].paused, "turn-budget error pauses the outbox");

    // A user turn resets the host budget: the event un-pauses and re-drains.
    d.handle(handlers::Input::Event(handlers::HostEvent::AgentTurnEnd { session: "sess-x".to_string() }), 2_000);
    let n = &d.project("auth").unwrap().outbox[0];
    assert!(n.sent, "after agent.turn_end the paused notice is retried and sent");
    assert!(!n.paused);
    assert_eq!(call_count(&d.host, "chat.prompt"), 2);
}

// ---------------------------------------------------------------------------
// 7. panel push size guard (10.2)
// ---------------------------------------------------------------------------

#[test]
fn oversized_snapshot_falls_back_to_summary_mode_truncated() {
    let (store, _dir) = temp_store();
    let host = FakeHost::new();
    let mut d = driver(store, host);

    let mut t = task("auth/t1", TaskState::Backlog);
    t.last_report = Some("x".repeat(1_000_000)); // > 900KB Full envelope
    d.insert_for_test(project("auth", ProjectPhase::Running, vec![t]), 1_000);

    d.push_board(2_000, true);

    let (panel, envelope) = d.host.panel_pushes.last().expect("a board push");
    assert_eq!(panel, "board");
    assert_eq!(envelope.get("kind").and_then(Value::as_str), Some("snapshot"));
    assert_eq!(envelope.get("truncated").and_then(Value::as_bool), Some(true));
    // Summary mode drops the report/description bodies.
    let task0 = &envelope["projects"][0]["tasks"][0];
    assert!(task0.get("lastReport").is_none(), "summary mode omits the report body");
    assert!(task0.get("description").is_none(), "summary mode omits the description body");
    // Counts/states are still present.
    assert_eq!(task0.get("state").and_then(Value::as_str), Some("backlog"));
}

#[test]
fn normal_snapshot_is_full_mode_not_truncated() {
    let (store, _dir) = temp_store();
    let host = FakeHost::new();
    let mut d = driver(store, host);
    d.insert_for_test(project("auth", ProjectPhase::Running, vec![task("auth/t1", TaskState::Todo)]), 1_000);

    d.push_board(2_000, true);

    let (_p, envelope) = d.host.panel_pushes.last().expect("a board push");
    assert_eq!(envelope.get("truncated").and_then(Value::as_bool), Some(false));
    assert!(envelope["projects"][0]["tasks"][0].get("description").is_some(), "full mode carries bodies");
}
