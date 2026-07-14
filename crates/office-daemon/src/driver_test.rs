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
        }),
        2_000,
    );

    assert_eq!(d.project("auth").unwrap().config.max_workers, 2, "untouched: the target project isn't owned/found");
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
fn persona_reply_with_prd_fence_lands_prd_and_kicks_breakdown() {
    // Live-test 2026-07-15: the persona narrated "handoff complete" forever while
    // prd_markdown stayed empty and the board never filled. A ```prd fence in the
    // reply must land as the PRD and immediately trigger the breakdown invoke.
    let (store, _dir) = temp_store();
    let mut d = driver(store, FakeHost::new());
    let fake = FakeInvoker::default();
    d.set_invoker(Box::new(fake.clone()));

    d.insert_for_test(drafting("a"), 1_000);
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
    let jobs = fake.jobs.lock().unwrap();
    assert!(
        jobs.iter().any(|j| j.purpose == InvokePurpose::Breakdown && j.proj_slug == "a"),
        "breakdown invoke kicked off automatically after PRD capture"
    );
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
