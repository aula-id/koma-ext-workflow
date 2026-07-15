//! Driver tests (BUILD_WAVES.md W7), all FakeHost-scripted + a tempfile store so the tick
//! loop, effect execution, and reconciliation are exercised with zero live host.
//!
//! `driver_test` is an inline submodule of `driver.rs`, so it may reach the driver's
//! private methods (`reconcile`, `drain_outbox`, `step`, ...) directly — the tests drive
//! those units without going through the 1s `recv_timeout` loop.

use super::*;
use crate::handlers;
use crate::host::FakeHost;
use office_core::digest::OfficeActivity;
use office_core::{
    AgentBinding, AgentKind, ChatAuthor, InvokePurpose, OutboundNotice, Project, ProjectConfig,
    ProjectPhase, Task, TaskId, TaskState,
};
use office_store::Store;
use std::sync::{Arc, Mutex as StdMutex};

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
        persona: String::new(),
    }
}

fn project(slug: &str, phase: ProjectPhase, tasks: Vec<Task>) -> Project {
    Project {
        id: office_core::ProjectId(slug.to_string()),
        name: format!("Project {slug}"),
        phase,
        prd_markdown: "# PRD\n".to_string(),
        trd_markdown: String::new(),
        research_notes: String::new(),
        research: None,
        crd_markdown: String::new(),
        audit: None,
        audit_rounds: 0,
        last_audit_grade: None,
        pending_assumptions: vec![],
        assumption_rounds: 0,
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
    // The worker spawns under its per-task persona id (office-worker-<name>), stably hashed
    // from the task id "auth/t1".
    let expected_agent = office_core::persona::worker_agent_id("auth/t1");
    assert_eq!(spawn.get("agent").and_then(Value::as_str), Some(expected_agent.as_str()));
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
// 2b. mid-run comment injection (feature 4): agents.send + CommentDelivered feedback
// ---------------------------------------------------------------------------

#[test]
fn comment_on_live_worker_injected_via_agents_send_and_marked_delivered() {
    let (store, _dir) = temp_store();
    let mut host = FakeHost::new();
    host.script("agents.send", json!({ "sent": true }));
    let mut d = driver(store, host);

    let t = task(
        "auth/t1",
        TaskState::OnProgress { binding: worker_binding(101, 1_000), attempt: 1 },
    );
    d.insert_for_test(project("auth", ProjectPhase::Running, vec![t]), 1_000);

    d.handle(
        handlers::Input::Command(handlers::Command::Comment {
            task: "auth/t1".to_string(),
            text: "watch the race".to_string(),
        }),
        2_000,
    );

    // The comment was pushed to the live worker (101) via agents.send with the framed message.
    assert_eq!(call_count(&d.host, "agents.send"), 1);
    let send = last_call(&d.host, "agents.send").unwrap();
    assert_eq!(send.get("agentId").and_then(Value::as_u64), Some(101));
    assert!(send
        .get("message")
        .and_then(Value::as_str)
        .unwrap()
        .contains("watch the race"));

    // The success reply flipped the receipt Pending -> Delivered.
    match &d.project("auth").unwrap().tasks[0].comments[0].receipt {
        office_core::Receipt::Delivered { .. } => {}
        other => panic!("expected Delivered, got {other:?}"),
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

// ---------------------------------------------------------------------------
// 7.5 config_set (10.2): direct project-config edit
// ---------------------------------------------------------------------------

#[test]
fn config_set_applies_partial_update_including_keep_desks() {
    let (store, _dir) = temp_store();
    let host = FakeHost::new();
    let mut d = driver(store, host);
    d.insert_for_test(project("auth", ProjectPhase::Running, vec![task("auth/t1", TaskState::Todo)]), 1_000);
    assert!(!d.project("auth").unwrap().config.keep_desks, "default is false");

    d.handle(
        handlers::Input::Command(handlers::Command::ConfigSet {
            project: "auth".to_string(),
            max_workers: Some(3),
            bounce_budget: None,
            worker_model: None,
            reviewer_model: None,
            keep_desks: Some(true),
            crd_pass_grade: None,
            assumption_check: None,
            assumption_mode: None,
        }),
        2_000,
    );

    let cfg = &d.project("auth").unwrap().config;
    assert_eq!(cfg.max_workers, 3, "provided field is applied");
    assert_eq!(cfg.bounce_budget, 3, "absent field keeps the default untouched");
    assert!(cfg.keep_desks, "keepDesks now parses end-to-end into ProjectConfig");

    // Persisted too, not just in-memory (10.2 write-through).
    let reloaded = d.store.load_project("auth").unwrap();
    assert!(reloaded.config.keep_desks);
}

#[test]
fn config_set_max_workers_is_clamped_to_the_project_ceiling() {
    let (store, _dir) = temp_store();
    let host = FakeHost::new();
    let mut d = driver(store, host);
    d.insert_for_test(project("auth", ProjectPhase::Running, vec![task("auth/t1", TaskState::Todo)]), 1_000);

    d.handle(
        handlers::Input::Command(handlers::Command::ConfigSet {
            project: "auth".to_string(),
            max_workers: Some(99),
            bounce_budget: None,
            worker_model: None,
            reviewer_model: None,
            keep_desks: None,
            crd_pass_grade: None,
            assumption_check: None,
            assumption_mode: None,
        }),
        2_000,
    );

    assert_eq!(d.project("auth").unwrap().config.max_workers, 4, "clamped to MAX_PROJECT_WORKERS");
}

#[test]
fn config_set_on_a_non_owned_project_is_dropped_not_applied() {
    let (store, _dir) = temp_store();
    let host = FakeHost::new();
    let mut d = driver(store, host);
    d.insert_for_test(project("auth", ProjectPhase::Running, vec![task("auth/t1", TaskState::Todo)]), 1_000);

    d.handle(
        handlers::Input::Command(handlers::Command::ConfigSet {
            project: "does-not-exist".to_string(),
            max_workers: Some(3),
            bounce_budget: None,
            worker_model: None,
            reviewer_model: None,
            keep_desks: Some(true),
            crd_pass_grade: None,
            assumption_check: None,
            assumption_mode: None,
        }),
        2_000,
    );

    assert_eq!(d.project("auth").unwrap().config.max_workers, 2, "untouched: the target project isn't owned/found");
}

// ---------------------------------------------------------------------------
// 7b. manual project delete (Settings "danger zone", 10.2 project_archive)
// ---------------------------------------------------------------------------

#[test]
fn project_archive_kills_bindings_removes_project_and_deletes_state() {
    let (store, _dir) = temp_store();
    let mut host = FakeHost::new();
    host.script("agents.kill", json!({ "killed": true }));
    let mut d = driver(store, host);

    let t = task(
        "auth/t1",
        TaskState::OnProgress { binding: worker_binding(101, 1_000), attempt: 1 },
    );
    d.insert_for_test(project("auth", ProjectPhase::Running, vec![t]), 1_000);
    assert!(d.holds_lease("auth"));

    d.handle(
        handlers::Input::Command(handlers::Command::ProjectArchive { project: "auth".to_string() }),
        2_000,
    );

    assert_eq!(call_count(&d.host, "agents.kill"), 1, "the in-flight binding is killed");
    assert_eq!(
        last_call(&d.host, "agents.kill").unwrap().get("agentId").and_then(Value::as_u64),
        Some(101)
    );

    assert!(d.project("auth").is_none(), "the project is removed from memory");
    assert!(
        d.projects_for_test().iter().all(|p| p.id.0 != "auth"),
        "removed from projects_for_test too"
    );
    assert!(d.store.load_project("auth").is_err(), "state.json is no longer loadable");
    assert!(
        d.store.registry().unwrap().iter().all(|r| r.project_id != "auth"),
        "the registry row is removed"
    );
}

#[test]
fn project_archive_on_a_non_owned_project_is_dropped_project_still_present() {
    let (store, _dir) = temp_store();
    // "other" is in the store (registry + state.json) but this driver never acquires its
    // lease (loaded via `driver()`, not `insert_for_test`/`bootstrap`), so it is owned by
    // no one here — an archive request for it must be silently dropped, same as
    // `config_set_on_a_non_owned_project_is_dropped_not_applied` above.
    store
        .save_project(&project("other", ProjectPhase::Running, vec![]))
        .expect("seed other");
    let host = FakeHost::new();
    let mut d = driver(store, host);
    assert!(!d.holds_lease("other"), "we must not own 'other'");

    d.handle(
        handlers::Input::Command(handlers::Command::ProjectArchive { project: "other".to_string() }),
        2_000,
    );

    assert_eq!(call_count(&d.host, "agents.kill"), 0, "no bindings are killed for a dropped archive");
    assert!(d.project("other").is_some(), "a non-owned archive must be dropped, not applied");
    assert!(d.store.load_project("other").is_ok(), "state.json must remain on disk");
}

// ---------------------------------------------------------------------------
// 8. off-loop invoke pool (W9, ARCHITECTURE.md 5.1 / 6.2)
// ---------------------------------------------------------------------------

/// Recording fake for the invoke pool: `run` just captures the job (no thread, no host)
/// so a test can inspect what was submitted and hand results back deterministically via
/// `d.handle(InvokeDone)`.
#[derive(Clone, Default)]
struct FakeInvoker {
    jobs: Arc<StdMutex<Vec<InvokeJob>>>,
}
impl Invoker for FakeInvoker {
    fn run(&mut self, job: InvokeJob) {
        self.jobs.lock().unwrap().push(job);
    }
}

fn drafting(slug: &str) -> Project {
    project(slug, ProjectPhase::Drafting, vec![])
}

/// A Drafting project with the 6.2c safeguard gate disabled, so a captured PRD/TRD/CRD fence
/// proceeds STRAIGHT to its pipeline stage (research/CRD/breakdown). Used to isolate the
/// research/TRD/CRD pipeline mechanics from the assumption gate, which has its own dedicated
/// wiring test (`prd_fence_runs_assume_check_then_spawns_research`).
fn drafting_no_gate(slug: &str) -> Project {
    let mut p = drafting(slug);
    p.config.assumption_check = false;
    p
}

fn invoke_done(req_id: u64, result: Result<String, String>) -> handlers::Input {
    handlers::Input::Command(handlers::Command::InvokeDone { req_id, result })
}

#[test]
fn brief_runs_invoke_off_loop_and_result_lands_in_transcript_without_starving_dispatch() {
    let (store, _dir) = temp_store();
    let mut host = FakeHost::new();
    // The only host calls in this flow are B's dispatch — never `models.invoke`.
    host.script("sessions.spawn_into", json!({ "agentId": 101, "status": "spawned" }));
    let mut d = driver(store, host);

    let fake = FakeInvoker::default();
    d.set_invoker(Box::new(fake.clone()));

    // A: the Drafting project we brief. B: a Running project with a ready task.
    d.insert_for_test(drafting("a"), 1_000);
    d.insert_for_test(project("b", ProjectPhase::Running, vec![task("b/t1", TaskState::Todo)]), 1_000);

    // Brief the office: the kernel emits an InvokeModel the driver hands OFF-loop.
    d.handle(
        handlers::Input::Command(handlers::Command::Brief {
            project: Some("a".to_string()),
            message: "build a crawler".to_string(),
        }),
        1_000,
    );

    // Off-loop: exactly one job queued, and NO inline models.invoke host call.
    {
        let jobs = fake.jobs.lock().unwrap();
        assert_eq!(jobs.len(), 1, "one persona invoke submitted to the pool");
        assert_eq!(jobs[0].purpose, InvokePurpose::Persona);
        assert_eq!(jobs[0].proj_slug, "a");
    }
    assert_eq!(call_count(&d.host, "models.invoke"), 0, "the invoke never runs inline on the tick loop");

    // The tick loop is NOT starved by the in-flight invoke: B still dispatches.
    d.on_tick(1_000);
    assert!(
        d.host.calls.iter().any(|(m, _)| m == "sessions.spawn_into"),
        "dispatch keeps running while an invoke is in flight"
    );
    // And a panel read (hello) is answerable without blocking.
    let _ = super::cache_snapshot();

    // The invoke returns: the office reply lands in A's transcript.
    let req_id = fake.jobs.lock().unwrap()[0].req_id;
    d.handle(invoke_done(req_id, Ok("Sure, here is my plan.".to_string())), 2_000);
    let a = d.project("a").unwrap();
    assert_eq!(a.office_transcript.len(), 2, "user turn + office reply");
    assert_eq!(a.office_transcript[1].who, ChatAuthor::Office);
    assert_eq!(a.office_transcript[1].text, "Sure, here is my plan.");

    // Main-chat-first trio: the office reply is ALSO queued to the chat.prompt
    // outbox (live-test 2026-07-15: replies used to land only in the panel
    // transcript, leaving the koma chat silent after "answer will arrive via chat").
    assert!(
        a.outbox.iter().any(|n| n.text.contains("Sure, here is my plan.") && n.text.starts_with("office[a]:")),
        "office reply flows to the koma chat via the outbox"
    );
}

#[test]
fn persona_reply_with_prd_fence_lands_prd_and_kicks_research() {
    // The PRD -> research -> TRD -> breakdown pipeline (ARCHITECTURE.md 6.2b): a ```prd fence
    // in a Drafting reply must land as the PRD and kick off the office-researcher spawn — NOT
    // the breakdown directly (that used to be the flow; it now waits for research + TRD).
    let (store, _dir) = temp_store();
    let mut host = FakeHost::new();
    host.script("sessions.spawn_into", json!({ "agentId": 700, "status": "spawned" }));
    let mut d = driver(store, host);
    let fake = FakeInvoker::default();
    d.set_invoker(Box::new(fake.clone()));

    d.insert_for_test(drafting_no_gate("a"), 1_000);
    d.handle(
        handlers::Input::Command(handlers::Command::Brief {
            project: Some("a".to_string()),
            message: "simple todo app".to_string(),
        }),
        1_000,
    );
    let req_id = fake.jobs.lock().unwrap()[0].req_id;
    d.handle(
        invoke_done(
            req_id,
            Ok("Agreed. Here it is:\n```prd\n# Todo App\nSimple Vite+React todo.\n```\nShall we?".to_string()),
        ),
        2_000,
    );

    let a = d.project("a").unwrap();
    assert_eq!(a.prd_markdown, "# Todo App\nSimple Vite+React todo.");
    assert!(
        a.outbox.iter().any(|n| n.text.contains("PRD drafted")),
        "chat notice announces the captured PRD"
    );
    // The ```prd fence kicks the office-researcher spawn (same host path as workers).
    let spawn = last_call(&d.host, "sessions.spawn_into").expect("a research spawn");
    assert_eq!(spawn.get("agent").and_then(Value::as_str), Some("office-researcher"));
    assert_eq!(spawn.get("notify").and_then(Value::as_bool), Some(true));
    match &a.research {
        Some(b) => assert_eq!(b.ext_agent_id, 700, "the real research agent id is recorded on the project"),
        None => panic!("a research binding must be recorded after the spawn"),
    }
    // No breakdown or TRD invoke yet — those wait until the researcher finishes.
    let jobs = fake.jobs.lock().unwrap();
    assert!(
        jobs.iter().all(|j| j.purpose != InvokePurpose::Breakdown && j.purpose != InvokePurpose::Trd),
        "breakdown/TRD do not run until after research completes"
    );
}

#[test]
fn research_agents_done_fetches_findings_and_drafts_the_trd() {
    // Full research leg (6.2b): PRD fence -> research spawn (id recorded) -> agents.done ->
    // agents.result -> findings stored -> TRD invoke submitted off-loop.
    let (store, _dir) = temp_store();
    let mut host = FakeHost::new();
    host.script("sessions.spawn_into", json!({ "agentId": 700, "status": "spawned" }));
    host.script("agents.result", json!({ "agentId": 700, "output": "OFFICE-RESEARCH\nfindings: - use axum 0.7\n" }));
    let mut d = driver(store, host);
    let fake = FakeInvoker::default();
    d.set_invoker(Box::new(fake.clone()));

    d.insert_for_test(drafting_no_gate("a"), 1_000);
    d.handle(
        handlers::Input::Command(handlers::Command::Brief {
            project: Some("a".to_string()),
            message: "build a service".to_string(),
        }),
        1_000,
    );
    let req_id = fake.jobs.lock().unwrap()[0].req_id;
    d.handle(invoke_done(req_id, Ok("```prd\n# Service\nBuild it.\n```".to_string())), 2_000);
    match &d.project("a").unwrap().research {
        Some(b) => assert_eq!(b.ext_agent_id, 700, "the real research agent id is recorded"),
        None => panic!("a research binding must be recorded"),
    }

    // The private agents.done notify for the researcher correlates to project "a" via the
    // research binding, fetches the findings, stores them, and drafts the TRD.
    d.handle(
        handlers::Input::Event(handlers::HostEvent::AgentsDone { agent_id: 700, status: "done".to_string() }),
        3_000,
    );
    let a = d.project("a").unwrap();
    assert!(a.research_notes.contains("axum 0.7"), "findings parsed and stored");
    assert!(a.research.is_none(), "binding cleared after findings land");
    assert_eq!(call_count(&d.host, "agents.result"), 1, "findings fetched via agents.result");
    assert!(
        fake.jobs.lock().unwrap().iter().any(|j| j.purpose == InvokePurpose::Trd && j.proj_slug == "a"),
        "the TRD invoke is submitted after research completes"
    );
}

#[test]
fn reconcile_runtime_ceiling_kills_over_age_researcher_and_degrades() {
    // Reconcile must cover a Drafting project's research binding (6.2b): an over-age
    // researcher is force-killed and Drafting degrades to a PRD-only TRD.
    let (store, _dir) = temp_store();
    let mut host = FakeHost::new();
    host.script("agents.kill", json!({ "killed": true }));
    host.script("agents.list", json!([]));
    let mut d = driver(store, host);
    let fake = FakeInvoker::default();
    d.set_invoker(Box::new(fake.clone()));

    let mut p = drafting("a");
    p.research = Some(AgentBinding {
        ext_agent_id: 700,
        session: "sess-x".to_string(),
        spawned_at_ms: 0,
        kind: AgentKind::Researcher,
        persona: String::new(),
    });
    d.insert_for_test(p, 0);

    let now = 21 * 60 * 1000; // past the 20-minute ceiling
    d.reconcile(now);

    assert_eq!(call_count(&d.host, "agents.kill"), 1, "the over-age researcher is force-killed");
    assert_eq!(
        last_call(&d.host, "agents.kill").unwrap().get("agentId").and_then(Value::as_u64),
        Some(700)
    );
    let a = d.project("a").unwrap();
    assert!(a.research.is_none(), "the researcher binding is cleared");
    assert!(
        fake.jobs.lock().unwrap().iter().any(|j| j.purpose == InvokePurpose::Trd),
        "Drafting degrades to a PRD-only TRD invoke"
    );
}

// ---------------------------------------------------------------------------
// 8a. office_activity() derivation (live "office activity" for the panel/MCP status)
// ---------------------------------------------------------------------------

fn invoke_job(req_id: u64, proj_slug: &str, purpose: InvokePurpose, submitted_at_ms: u64) -> InvokeJob {
    InvokeJob {
        req_id,
        submitted_at_ms,
        proj_slug: proj_slug.to_string(),
        purpose,
        role: String::new(),
        system: String::new(),
        prompt: String::new(),
        retried: false,
        format: None,
    }
}

fn research_binding(agent_id: u64, spawned_at_ms: u64) -> AgentBinding {
    AgentBinding {
        ext_agent_id: agent_id,
        session: "sess-x".to_string(),
        spawned_at_ms,
        kind: AgentKind::Researcher,
        persona: String::new(),
    }
}

fn audit_binding(agent_id: u64, spawned_at_ms: u64) -> AgentBinding {
    AgentBinding {
        ext_agent_id: agent_id,
        session: "sess-x".to_string(),
        spawned_at_ms,
        kind: AgentKind::Auditor,
        persona: String::new(),
    }
}

#[test]
fn office_activity_labels_every_invoke_purpose() {
    let cases: Vec<(InvokePurpose, &str)> = vec![
        (InvokePurpose::Persona, "office is replying"),
        (InvokePurpose::Fold, "summarizing the conversation"),
        (InvokePurpose::AssumeCheckPrd, "fact-checking the PRD"),
        (InvokePurpose::AssumeCheckTrd, "fact-checking the TRD"),
        (InvokePurpose::AssumeCheckCrd, "fact-checking the CRD"),
        (InvokePurpose::AssumeResolve, "resolving assumptions"),
        (InvokePurpose::Trd, "drafting the TRD"),
        (InvokePurpose::Crd, "drafting the CRD"),
        (InvokePurpose::Breakdown, "breaking down the plan"),
        (InvokePurpose::BreakdownReask, "breaking down the plan"),
        (InvokePurpose::BreakdownCompact, "breaking down the plan"),
    ];
    for (purpose, expected_label) in cases {
        let mut pending: HashMap<u64, InvokeJob> = HashMap::new();
        pending.insert(1, invoke_job(1, "a", purpose, 500));
        let p = drafting("a");
        let activity = office_activity(&pending, &p).expect("an activity for a pending invoke");
        assert_eq!(activity.label, expected_label, "purpose {purpose:?}");
        assert_eq!(activity.since_ms, 500);
    }
}

#[test]
fn office_activity_research_only_yields_research_label() {
    let pending: HashMap<u64, InvokeJob> = HashMap::new();
    let mut p = drafting("a");
    p.research = Some(research_binding(700, 1_000));
    let activity = office_activity(&pending, &p).expect("research activity");
    assert_eq!(activity.label, "researching the stack");
    assert_eq!(activity.since_ms, 1_000);
}

#[test]
fn office_activity_audit_only_yields_audit_label() {
    let pending: HashMap<u64, InvokeJob> = HashMap::new();
    let mut p = drafting("a");
    p.audit = Some(audit_binding(701, 2_000));
    let activity = office_activity(&pending, &p).expect("audit activity");
    assert_eq!(activity.label, "auditing the delivery");
    assert_eq!(activity.since_ms, 2_000);
}

#[test]
fn office_activity_pending_invoke_wins_over_research_and_audit() {
    let mut pending: HashMap<u64, InvokeJob> = HashMap::new();
    pending.insert(1, invoke_job(1, "a", InvokePurpose::Trd, 3_000));
    let mut p = drafting("a");
    p.research = Some(research_binding(700, 1_000));
    p.audit = Some(audit_binding(701, 2_000));
    let activity = office_activity(&pending, &p).expect("invoke activity wins");
    assert_eq!(activity.label, "drafting the TRD");
    assert_eq!(activity.since_ms, 3_000);
}

#[test]
fn office_activity_research_wins_over_audit_when_both_live() {
    let pending: HashMap<u64, InvokeJob> = HashMap::new();
    let mut p = drafting("a");
    p.research = Some(research_binding(700, 1_000));
    p.audit = Some(audit_binding(701, 2_000));
    let activity = office_activity(&pending, &p).expect("research wins over audit");
    assert_eq!(activity.label, "researching the stack");
    assert_eq!(activity.since_ms, 1_000);
}

#[test]
fn office_activity_none_when_nothing_live() {
    let pending: HashMap<u64, InvokeJob> = HashMap::new();
    let p = drafting("a");
    assert_eq!(office_activity(&pending, &p), None::<OfficeActivity>);
}

#[test]
fn office_activity_waiting_on_user_when_pending_assumptions_and_nothing_else_live() {
    // Feature 5: no invoke/research/audit is live but the drafting pipeline is STOPPED on the
    // safeguard's pending assumptions -> a "waiting on you — N assumptions" label with a 0
    // sentinel timestamp (the UI hides the elapsed suffix when since_ms == 0).
    let pending: HashMap<u64, InvokeJob> = HashMap::new();
    let mut p = drafting("a");
    p.pending_assumptions = vec!["assumed Postgres".to_string(), "assumed React".to_string()];
    let activity = office_activity(&pending, &p).expect("a waiting-on-user activity");
    assert_eq!(activity.label, "waiting on you — 2 assumptions");
    assert_eq!(activity.since_ms, 0);
}

#[test]
fn office_activity_singular_assumption_label() {
    let pending: HashMap<u64, InvokeJob> = HashMap::new();
    let mut p = drafting("a");
    p.pending_assumptions = vec!["assumed Postgres".to_string()];
    let activity = office_activity(&pending, &p).expect("a waiting-on-user activity");
    assert_eq!(activity.label, "waiting on you — 1 assumption");
}

#[test]
fn office_activity_live_invoke_wins_over_waiting_on_user() {
    // A live invoke still wins over the waiting state — the office is actively working again.
    let mut pending: HashMap<u64, InvokeJob> = HashMap::new();
    pending.insert(1, invoke_job(1, "a", InvokePurpose::Persona, 500));
    let mut p = drafting("a");
    p.pending_assumptions = vec!["assumed Postgres".to_string()];
    let activity = office_activity(&pending, &p).expect("invoke activity wins");
    assert_eq!(activity.label, "office is replying");
}

// ---------------------------------------------------------------------------
// 8b. safeguard no-assume gate wiring (6.2c feature C)
// ---------------------------------------------------------------------------

#[test]
fn prd_fence_runs_assume_check_off_loop_on_safeguard_role_then_research_on_clean() {
    // With the gate ON (default) a captured PRD submits an AssumeCheck invoke OFF-loop on the
    // safeguard role — no research spawn yet. A clean check then spawns the researcher.
    let (store, _dir) = temp_store();
    let mut host = FakeHost::new();
    host.script("sessions.spawn_into", json!({ "agentId": 700, "status": "spawned" }));
    let mut d = driver(store, host);
    let fake = FakeInvoker::default();
    d.set_invoker(Box::new(fake.clone()));

    d.insert_for_test(drafting("a"), 1_000); // gate ON (default config)
    d.handle(
        handlers::Input::Command(handlers::Command::Brief {
            project: Some("a".to_string()),
            message: "simple todo app".to_string(),
        }),
        1_000,
    );
    let persona_id = fake.jobs.lock().unwrap()[0].req_id;
    d.handle(invoke_done(persona_id, Ok("```prd\n# Todo\nBuild it.\n```".to_string())), 2_000);

    // The gate invoke was submitted on the safeguard role; NO research spawn happened.
    let gate_id = {
        let jobs = fake.jobs.lock().unwrap();
        let gate = jobs
            .iter()
            .find(|j| j.purpose == InvokePurpose::AssumeCheckPrd)
            .expect("an assume-check invoke was submitted");
        assert_eq!(gate.role, "safeguard", "the gate runs on the safeguard role");
        gate.req_id
    };
    assert!(d.project("a").unwrap().research.is_none(), "research is gated behind the check");
    assert_eq!(call_count(&d.host, "sessions.spawn_into"), 0, "no research spawn until the check clears");

    // A clean check -> the researcher spawns (same host path as before the gate existed).
    d.handle(invoke_done(gate_id, Ok("ASSUME-CHECK\nverdict: clean\n".to_string())), 3_000);
    let spawn = last_call(&d.host, "sessions.spawn_into").expect("a research spawn after a clean check");
    assert_eq!(spawn.get("agent").and_then(Value::as_str), Some("office-researcher"));
    assert!(d.project("a").unwrap().research.is_some());
}

// ---------------------------------------------------------------------------
// 8c. clean-build auditor spawn wiring (6.2c feature B)
// ---------------------------------------------------------------------------

fn reviewer_binding(agent_id: u64, spawned_at_ms: u64) -> AgentBinding {
    AgentBinding {
        ext_agent_id: agent_id,
        session: "sess-x".to_string(),
        spawned_at_ms,
        kind: AgentKind::Reviewer,
        persona: "office-reviewer".to_string(),
    }
}

#[test]
fn completion_with_crd_spawns_office_auditor_and_records_its_id() {
    let (store, _dir) = temp_store();
    let mut host = FakeHost::new();
    // The last reviewer passes, then the auditor is spawned into the bound session.
    host.script("agents.result", json!({ "agentId": 500, "output": "OFFICE-REVIEW\nverdict: pass\n" }));
    host.script("sessions.spawn_into", json!({ "agentId": 900, "status": "spawned" }));
    let mut d = driver(store, host);

    let mut p = project(
        "auth",
        ProjectPhase::Running,
        vec![task("auth/t1", TaskState::Review { binding: Some(reviewer_binding(500, 1_000)), attempt: 1 })],
    );
    p.crd_markdown = "# CRD\n- README present (100 pts)".to_string();
    d.insert_for_test(p, 1_000);

    // The reviewer finishes -> the last task is Done -> the auditor is spawned (not Done yet).
    d.handle(
        handlers::Input::Event(handlers::HostEvent::AgentsDone { agent_id: 500, status: "done".to_string() }),
        2_000,
    );

    let spawn = last_call(&d.host, "sessions.spawn_into").expect("an auditor spawn");
    assert_eq!(spawn.get("agent").and_then(Value::as_str), Some("office-auditor"));
    assert_eq!(spawn.get("notify").and_then(Value::as_bool), Some(true));
    assert_eq!(spawn.get("session").and_then(Value::as_str), Some("sess-x"));

    let a = d.project("auth").unwrap();
    assert!(matches!(a.phase, ProjectPhase::Running), "not Done — the audit gates completion");
    match &a.audit {
        Some(b) => {
            assert_eq!(b.ext_agent_id, 900, "the real auditor id is recorded on the project");
            assert_eq!(b.kind, AgentKind::Auditor);
        }
        None => panic!("an audit binding must be recorded after the spawn"),
    }
}

#[test]
fn auditor_cross_process_spawn_degrades_to_done_without_releasing_lease() {
    let (store, _dir) = temp_store();
    let mut host = FakeHost::new();
    host.script("agents.result", json!({ "agentId": 500, "output": "OFFICE-REVIEW\nverdict: pass\n" }));
    // The bound session moved off-daemon: the auditor spawn is a fire-and-forget `sent`.
    host.script("sessions.spawn_into", json!({ "status": "sent", "session": "sess-x" }));
    let mut d = driver(store, host);

    let mut p = project(
        "auth",
        ProjectPhase::Running,
        vec![task("auth/t1", TaskState::Review { binding: Some(reviewer_binding(500, 1_000)), attempt: 1 })],
    );
    p.crd_markdown = "# CRD".to_string();
    d.insert_for_test(p, 1_000);
    assert!(d.holds_lease("auth"));

    d.handle(
        handlers::Input::Event(handlers::HostEvent::AgentsDone { agent_id: 500, status: "done".to_string() }),
        2_000,
    );

    let a = d.project("auth").unwrap();
    assert!(matches!(a.phase, ProjectPhase::Done { .. }), "a cross-process auditor spawn degrades to Done");
    assert!(a.audit.is_none());
    // Unlike a worker cross-process spawn (5.6), an auditor degrade never releases the lease.
    assert!(d.holds_lease("auth"), "the audit degrade path keeps the lease");
}

#[test]
fn brief_with_unknown_project_id_mints_a_drafting_project_instead_of_dropping() {
    // Live-test bug 2026-07-15: `workflow_brief` with a fresh project id was acked
    // ("office is thinking") and then silently dropped because resolve_office_project
    // found nothing. A brief is the documented way to START a project, so an
    // unresolved id must mint a Drafting project and land the message in it.
    let (store, _dir) = temp_store();
    let mut d = driver(store, FakeHost::new());
    let fake = FakeInvoker::default();
    d.set_invoker(Box::new(fake.clone()));

    d.handle(
        handlers::Input::Command(handlers::Command::Brief {
            project: Some("todoapp-simple".to_string()),
            message: "Build a very simple todo app using Vite and React.".to_string(),
        }),
        1_000,
    );

    let p = d.project("todoapp-simple").expect("project was minted from the brief");
    assert!(matches!(p.phase, ProjectPhase::Drafting));
    assert_eq!(p.office_transcript.len(), 1, "the brief message landed in the transcript");
    assert_eq!(p.office_transcript[0].text, "Build a very simple todo app using Vite and React.");
    assert_eq!(fake.jobs.lock().unwrap().len(), 1, "persona invoke kicked off for the new project");
}

#[test]
fn brief_with_no_project_and_empty_board_mints_a_named_drafting_project() {
    let (store, _dir) = temp_store();
    let mut d = driver(store, FakeHost::new());
    let fake = FakeInvoker::default();
    d.set_invoker(Box::new(fake.clone()));

    d.handle(
        handlers::Input::Command(handlers::Command::Brief {
            project: None,
            message: "we want to make todo apps, very simple, using vite and react".to_string(),
        }),
        1_000,
    );

    // Name derives from the first words of the brief; project exists and is Drafting.
    let minted = d
        .projects_for_test()
        .iter()
        .find(|p| matches!(p.phase, ProjectPhase::Drafting))
        .cloned()
        .expect("a drafting project was minted");
    assert_eq!(minted.name, derive_project_name("we want to make todo apps, very simple, using vite and react"));
    assert_eq!(minted.office_transcript.len(), 1);
    assert_eq!(fake.jobs.lock().unwrap().len(), 1);
}

#[test]
fn derive_project_name_takes_leading_words_and_never_returns_empty() {
    assert_eq!(derive_project_name("build a crawler"), "build a crawler");
    assert_eq!(
        derive_project_name("we want to make todo apps, very simple, using vite and react"),
        "we want to make todo apps,"
    );
    assert_eq!(derive_project_name("   "), "untitled");
    assert!(derive_project_name(&"x".repeat(300)).len() <= 48);
}

#[test]
fn invoke_timeout_retries_exactly_once_then_surfaces() {
    let (store, _dir) = temp_store();
    let mut d = driver(store, FakeHost::new());
    let fake = FakeInvoker::default();
    d.set_invoker(Box::new(fake.clone()));
    d.insert_for_test(drafting("a"), 1_000);

    d.handle(
        handlers::Input::Command(handlers::Command::Brief {
            project: Some("a".to_string()),
            message: "hi".to_string(),
        }),
        1_000,
    );
    let req_id = fake.jobs.lock().unwrap()[0].req_id;

    // First timeout -> exactly one re-run of the SAME job (same slot), no kernel routing.
    d.handle(invoke_done(req_id, Err("model call timed out".to_string())), 1_100);
    {
        let jobs = fake.jobs.lock().unwrap();
        assert_eq!(jobs.len(), 2, "the timeout is retried exactly once");
        assert!(jobs[1].retried, "the retry is marked");
        assert_eq!(jobs[0].req_id, jobs[1].req_id, "the retry reuses the same request id");
    }
    assert_eq!(
        d.project("a").unwrap().office_transcript.len(),
        1,
        "no office reply yet — still just the user turn"
    );

    // Second timeout on the retried job -> no third submit; the outcome surfaces to the user.
    d.handle(invoke_done(req_id, Err("model call timed out".to_string())), 1_200);
    assert_eq!(fake.jobs.lock().unwrap().len(), 2, "no further retries after the first");
    let a = d.project("a").unwrap();
    assert_eq!(a.office_transcript.len(), 2, "the failure is surfaced as an office message");
    assert_eq!(a.office_transcript[1].who, ChatAuthor::Office);
}

#[test]
fn invoke_pool_cap_is_honored_and_queue_drains_on_completion() {
    let (store, _dir) = temp_store();
    let mut d = driver(store, FakeHost::new());
    let fake = FakeInvoker::default();
    d.set_invoker(Box::new(fake.clone()));

    // Three Drafting projects, each briefed -> three invokes; pool cap is 2.
    for slug in ["a", "b", "c"] {
        d.insert_for_test(drafting(slug), 1_000);
    }
    for slug in ["a", "b", "c"] {
        d.handle(
            handlers::Input::Command(handlers::Command::Brief {
                project: Some(slug.to_string()),
                message: "go".to_string(),
            }),
            1_000,
        );
    }

    assert_eq!(fake.jobs.lock().unwrap().len(), 2, "only INVOKE_POOL_CAP (2) run at once; the third queues");

    // Complete the first (req_id 1) -> a slot frees -> the queued third starts.
    d.handle(invoke_done(1, Ok("done".to_string())), 1_100);
    assert_eq!(fake.jobs.lock().unwrap().len(), 3, "the queued invoke starts when a slot frees");
}

// ---------------------------------------------------------------------------
// bootstrap lease acquisition (ARCHITECTURE.md 4.4: rebind is same-SESSION, not
// same-bound-project)
// ---------------------------------------------------------------------------

#[test]
fn bootstrap_never_steals_a_fresh_lease_held_under_a_different_daemon_session() {
    let (store, _dir) = temp_store();
    let slug = "auth";
    store
        .save_project(&project(slug, ProjectPhase::Running, vec![task("auth/t1", TaskState::Todo)]))
        .expect("save project");

    // Seed a live lease the way the REAL owning daemon would: instance "inst-owner",
    // written under its OWN session "sess-x" (which the project also happens to be bound
    // to, since it is the daemon actively dispatching it).
    let lease_path = store.lease_path(slug);
    lease::acquire(&lease_path, "inst-owner", Some("sess-x"), 900, 1_000)
        .expect("seed lease io")
        .expect("seed lease acquired");

    // A second daemon boots against the same shared state root under a DIFFERENT session
    // ("sess-y") with a brand-new instance id. It must never rebind a lease that belongs to
    // another daemon's session -- only a same-session restart may rebind. Before the fix,
    // `bootstrap` compared the project's `bound_session` ("sess-x") against itself instead
    // of this daemon's own session, so the rebind clause was a tautology and this steal
    // succeeded regardless of which session actually booted.
    let mut host = FakeHost::new();
    host.script("sessions.list", json!([{ "id": "sess-y", "workdir": "/ws" }]));
    host.script("agents.list", json!([]));
    let mut d = Driver::load(store, host, "inst-rival".to_string(), 111).expect("load driver");
    d.bootstrap(2_000); // well within STALE_MS (60s) of the seeded heartbeat

    assert!(
        d.projects[0].lease.is_none(),
        "a live lease bound to a different daemon session must not be stolen"
    );
    let on_disk = lease::read(&lease_path).unwrap().unwrap();
    assert_eq!(on_disk.instance, "inst-owner", "the foreign live lease must be left untouched on disk");
}

// ---------------------------------------------------------------------------
// project_create: the panel "New Project" op actually mints a Drafting project
// ---------------------------------------------------------------------------

#[test]
fn project_create_mints_drafting_project_persists_and_leases() {
    let (store, _dir) = temp_store();
    let host = FakeHost::new();
    let mut d = driver(store, host);
    assert!(d.projects.is_empty(), "fresh driver starts with no projects");

    d.handle(
        handlers::Input::Command(handlers::Command::ProjectCreate {
            name: "My New Project".to_string(),
        }),
        1_000,
    );

    // In-memory: exactly one project, slugged from the name, in Drafting with an empty
    // board and a default config. Before the fix this whole op was a no-op arm, so
    // `d.project(...)` was `None` and every assertion below failed.
    let p = d.project("my-new-project").expect("project_create must create a project");
    assert_eq!(p.name, "My New Project");
    assert_eq!(p.phase, ProjectPhase::Drafting);
    assert!(p.tasks.is_empty() && p.epics.is_empty() && p.stories.is_empty());
    assert_eq!(p.config, ProjectConfig::default_config());
    assert!(d.holds_lease("my-new-project"), "the creator must hold the new project's lease");

    // Durable: the project is on disk (state.json + registry row), so a fresh driver
    // reloading the same store root sees it.
    let reloaded = Driver::load(
        Store::open(_dir.path()).expect("reopen store"),
        FakeHost::new(),
        "reload-instance".to_string(),
        4243,
    )
    .expect("reload driver");
    assert!(
        reloaded.project("my-new-project").is_some(),
        "the created project must survive a reload from the store"
    );

    // The panel got a repaint carrying the new project.
    let (panel_id, envelope) = d.host.panel_pushes.last().expect("a board push");
    assert_eq!(panel_id, "board");
    let ids: Vec<&str> = envelope["projects"]
        .as_array()
        .expect("projects array")
        .iter()
        .filter_map(|p| p["id"].as_str())
        .collect();
    assert!(ids.contains(&"my-new-project"), "the pushed snapshot must include the new project");
}

#[test]
fn project_create_dedupes_colliding_slugs() {
    let (store, _dir) = temp_store();
    let host = FakeHost::new();
    let mut d = driver(store, host);

    for _ in 0..2 {
        d.handle(
            handlers::Input::Command(handlers::Command::ProjectCreate { name: "Same Name".to_string() }),
            1_000,
        );
    }

    assert!(d.project("same-name").is_some(), "first create takes the bare slug");
    assert!(d.project("same-name-2").is_some(), "second create must not clobber the first");
    assert_eq!(d.projects.len(), 2, "both projects exist independently");
}

// ---------------------------------------------------------------------------
// global inbox: ownership-aware claiming (shared ~/.koma-workflow/inbox)
// ---------------------------------------------------------------------------

fn write_global_file(store: &Store, name: &str, body: &str) {
    let dir = store.root_dir().join("inbox");
    std::fs::create_dir_all(&dir).expect("create global inbox");
    std::fs::write(dir.join(name), body).expect("write global inbox file");
}

#[test]
fn global_inbox_claims_file_for_owned_project() {
    let (store, _dir) = temp_store();
    let host = FakeHost::new();
    let mut d = driver(store, host);
    d.insert_for_test(project("mine", ProjectPhase::Running, vec![]), 1_000);
    assert!(d.holds_lease("mine"));

    write_global_file(&d.store, "1-resume.json", r#"{"op":"resume","project":"mine"}"#);
    d.poll_global_inbox(2_000);

    // Consumed into processed/, and an acknowledgement chat.prompt was sent.
    let inbox = d.store.root_dir().join("inbox");
    assert!(inbox.join("processed").join("1-resume.json").exists());
    assert!(!inbox.join("1-resume.json").exists());
    assert!(call_count(&d.host, "chat.prompt") >= 1, "an ack is sent for a claimed file");
}

#[test]
fn global_inbox_leaves_file_for_unowned_project() {
    let (store, _dir) = temp_store();
    // "other" is in the store (registry) but this driver never acquires its lease, so it
    // is owned by no one here — it must be left for the instance that does own it.
    store
        .save_project(&project("other", ProjectPhase::Running, vec![]))
        .expect("seed other");
    let host = FakeHost::new();
    let mut d = driver(store, host);
    assert!(!d.holds_lease("other"), "we must not own 'other'");

    write_global_file(&d.store, "1-resume.json", r#"{"op":"resume","project":"other"}"#);
    d.poll_global_inbox(2_000);

    // Left in place; no ack for a file we did not claim.
    let inbox = d.store.root_dir().join("inbox");
    assert!(inbox.join("1-resume.json").exists(), "an unowned file is left in place");
    assert!(!inbox.join("processed").join("1-resume.json").exists());
    assert_eq!(call_count(&d.host, "chat.prompt"), 0);
}

/// `breakdown` is owner-only exactly like `authorize`/`resume` (6.4): this instance may
/// claim a `breakdown` file addressed to a project it owns...
#[test]
fn global_inbox_claims_breakdown_for_owned_project() {
    let (store, _dir) = temp_store();
    let host = FakeHost::new();
    let mut d = driver(store, host);
    d.insert_for_test(project("mine", ProjectPhase::Drafting, vec![]), 1_000);
    assert!(d.holds_lease("mine"));

    write_global_file(&d.store, "1-breakdown.json", r#"{"op":"breakdown","project":"mine"}"#);
    d.poll_global_inbox(2_000);

    let inbox = d.store.root_dir().join("inbox");
    assert!(inbox.join("processed").join("1-breakdown.json").exists());
    assert!(!inbox.join("1-breakdown.json").exists());
    assert!(call_count(&d.host, "chat.prompt") >= 1, "an ack is sent for a claimed file");
}

/// ...and must LEAVE one addressed to a project owned by another instance.
#[test]
fn global_inbox_leaves_breakdown_for_unowned_project() {
    let (store, _dir) = temp_store();
    store
        .save_project(&project("other", ProjectPhase::Drafting, vec![]))
        .expect("seed other");
    let host = FakeHost::new();
    let mut d = driver(store, host);
    assert!(!d.holds_lease("other"), "we must not own 'other'");

    write_global_file(&d.store, "1-breakdown.json", r#"{"op":"breakdown","project":"other"}"#);
    d.poll_global_inbox(2_000);

    let inbox = d.store.root_dir().join("inbox");
    assert!(inbox.join("1-breakdown.json").exists(), "an unowned breakdown file is left in place");
    assert!(!inbox.join("processed").join("1-breakdown.json").exists());
    assert_eq!(call_count(&d.host, "chat.prompt"), 0);
}

#[test]
fn global_inbox_claims_unknown_project_brief_and_mints_locally() {
    let (store, _dir) = temp_store();
    let host = FakeHost::new();
    let mut d = driver(store, host);

    // No projects owned or known. A brief naming a brand-new project id is claimable by
    // anyone (it is not owned elsewhere) and mints a fresh Drafting project locally.
    write_global_file(
        &d.store,
        "1-brief.json",
        r#"{"op":"brief","project":"brandnew","message":"build a thing"}"#,
    );
    d.poll_global_inbox(2_000);

    assert!(d.project("brandnew").is_some(), "an unknown-project brief mints locally");
    let inbox = d.store.root_dir().join("inbox");
    assert!(inbox.join("processed").join("1-brief.json").exists());
    assert!(call_count(&d.host, "chat.prompt") >= 1);
}
