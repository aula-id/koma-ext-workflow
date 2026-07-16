//! Kernel tests (ARCHITECTURE.md 5.1-5.3 + BUILD_WAVES.md W4 test plan). The kernel
//! is the correctness core, so this is the heaviest test wave: dispatch/capacity,
//! runtime ceiling, receipt discipline, the full task lifecycle, bounce/park/halt,
//! interrupt/resume, and determinism.

use super::kernel::*;
use crate::domain::*;
use crate::office::InvokePurpose;
use std::path::PathBuf;

// ---------------------------------------------------------------------------
// Builders
// ---------------------------------------------------------------------------

fn task(id: &str, state: TaskState, priority: i32, blocked_by: &[&str]) -> Task {
    Task {
        id: TaskId(id.to_string()),
        title: format!("task {}", id),
        description: "do the thing".to_string(),
        acceptance: vec!["it works".to_string()],
        blocked_by: blocked_by.iter().map(|b| TaskId(b.to_string())).collect(),
        priority,
        state,
        bounces: 0,
        comments: Vec::new(),
        desk: None,
        last_report: None,
        last_review: None,
        history: Vec::new(),
    }
}

fn project(phase: ProjectPhase, tasks: Vec<Task>) -> Project {
    Project {
        id: ProjectId("proj".to_string()),
        name: "Proj".to_string(),
        phase,
        prd_markdown: "# PRD\nbuild it".to_string(),
        trd_markdown: String::new(),
        research_notes: String::new(),
        research: None,
        crd_markdown: String::new(),
        audit: None,
        audit_rounds: 0,
        last_audit_grade: None,
        pending_assumptions: Vec::new(),
        assumptions_approved: false,
        self_resolved_assumptions: Vec::new(),
        capture_nudge_count: 0,
        assumption_rounds: 0,
        office_transcript: Vec::new(),
        office_summary: String::new(),
        delivery_path: Some(PathBuf::from("/ws/deliver")),
        bound_session: Some("sess-1".to_string()),
        workspace: Some(PathBuf::from("/ws")),
        epics: Vec::new(),
        stories: Vec::new(),
        tasks,
        config: ProjectConfig::default_config(),
        outbox: Vec::new(),
        trace: Vec::new(),
        interrupted_from: None,
        gate_cleared: false,
        gate_invoke_live_hint: false,
        pending_breakdown: None,
        seq: 0,
    }
}

fn worker_binding(id: u64, at: u64) -> AgentBinding {
    AgentBinding {
        ext_agent_id: id,
        session: "sess-1".to_string(),
        spawned_at_ms: at,
        kind: AgentKind::Worker,
        persona: String::new(),
    }
}

fn reviewer_binding(id: u64, at: u64) -> AgentBinding {
    AgentBinding {
        ext_agent_id: id,
        session: "sess-1".to_string(),
        spawned_at_ms: at,
        kind: AgentKind::Reviewer,
        persona: "office-reviewer".to_string(),
    }
}

fn researcher_binding(id: u64, at: u64) -> AgentBinding {
    AgentBinding {
        ext_agent_id: id,
        session: "sess-1".to_string(),
        spawned_at_ms: at,
        kind: AgentKind::Researcher,
        persona: String::new(),
    }
}

fn auditor_binding(id: u64, at: u64) -> AgentBinding {
    AgentBinding {
        ext_agent_id: id,
        session: "sess-1".to_string(),
        spawned_at_ms: at,
        kind: AgentKind::Auditor,
        persona: String::new(),
    }
}

fn count_spawns(fx: &[Effect]) -> usize {
    fx.iter().filter(|e| matches!(e, Effect::Spawn { .. })).count()
}

fn spawn_agents<'a>(fx: &'a [Effect]) -> Vec<&'a str> {
    fx.iter()
        .filter_map(|e| match e {
            Effect::Spawn { agent, .. } => Some(agent.as_str()),
            _ => None,
        })
        .collect()
}

fn find_task<'a>(p: &'a Project, id: &str) -> &'a Task {
    p.tasks.iter().find(|t| t.id.0 == id).unwrap()
}

/// Mirror of the kernel's private `next_attempt` ledger read, so tests can assert
/// the attempt a task's next dispatch will use without exposing kernel internals.
fn next_attempt(t: &Task) -> u32 {
    t.history
        .iter()
        .rev()
        .find_map(|e| {
            e.event
                .strip_prefix("next-attempt:")
                .and_then(|s| s.trim().parse::<u32>().ok())
        })
        .unwrap_or(1)
}

const REPORT_OK: &str = "did the work\nOFFICE-REPORT\nstatus: complete\nsummary: built it\ndelivered: /ws/deliver/a.rs\n";
const REVIEW_PASS: &str = "looks good\nOFFICE-REVIEW\nverdict: pass\nreasons: meets acceptance\n";
const REVIEW_FAIL: &str = "OFFICE-REVIEW\nverdict: fail\nreasons: missing tests\n";

// ---------------------------------------------------------------------------
// Dispatch: deps, priority, session-global capacity
// ---------------------------------------------------------------------------

#[test]
fn dispatch_respects_blocked_by_deps() {
    // t2 blocked by t1 (not done) -> only t1 dispatches.
    let mut p = project(
        ProjectPhase::Running,
        vec![
            task("t1", TaskState::Todo, 0, &[]),
            task("t2", TaskState::Todo, 0, &["t1"]),
        ],
    );
    let fx = step(&mut p, Input::Host(HostEvent::Tick), 1000, 4);
    assert_eq!(count_spawns(&fx), 1);
    assert!(matches!(find_task(&p, "t1").state, TaskState::OnProgress { .. }));
    assert!(matches!(find_task(&p, "t2").state, TaskState::Todo));
}

#[test]
fn dispatch_priority_order_is_deterministic() {
    // Capacity for exactly one worker; the higher-priority task wins.
    let mut p = project(
        ProjectPhase::Running,
        vec![
            task("low", TaskState::Todo, 1, &[]),
            task("high", TaskState::Todo, 9, &[]),
        ],
    );
    p.config.max_workers = 1;
    let fx = step(&mut p, Input::Host(HostEvent::Tick), 1000, 4);
    assert_eq!(count_spawns(&fx), 1);
    assert!(matches!(find_task(&p, "high").state, TaskState::OnProgress { .. }));
    assert!(matches!(find_task(&p, "low").state, TaskState::Todo));
}

#[test]
fn dispatch_session_capacity_is_global_across_projects() {
    // Two Running projects share ONE session budget of 4. Project A holds all four
    // slots (max_workers 4, five ready tasks); B, given the remaining budget, spawns
    // nothing. Combined <= 4 and one host slot (of 5) is always left for the user.
    let mut a = project(
        ProjectPhase::Running,
        (0..5)
            .map(|i| task(&format!("a{}", i), TaskState::Todo, 0, &[]))
            .collect(),
    );
    a.config.max_workers = 4;
    let mut b = project(
        ProjectPhase::Running,
        (0..5)
            .map(|i| task(&format!("b{}", i), TaskState::Todo, 0, &[]))
            .collect(),
    );
    b.config.max_workers = 4;

    let mut cap = 4u32;
    let fx_a = step(&mut a, Input::Host(HostEvent::Tick), 1000, cap);
    let spawns_a = count_spawns(&fx_a) as u32;
    cap -= spawns_a;
    let fx_b = step(&mut b, Input::Host(HostEvent::Tick), 1000, cap);
    let spawns_b = count_spawns(&fx_b) as u32;

    assert_eq!(spawns_a, 4);
    assert_eq!(spawns_b, 0);
    assert!(spawns_a + spawns_b <= 4, "office must leave one host slot for the user");
}

#[test]
fn dispatch_max_workers_soft_ceiling_caps_a_single_project() {
    let mut p = project(
        ProjectPhase::Running,
        (0..5)
            .map(|i| task(&format!("t{}", i), TaskState::Todo, 0, &[]))
            .collect(),
    );
    p.config.max_workers = 2;
    let fx = step(&mut p, Input::Host(HostEvent::Tick), 1000, 4);
    // Session budget is 4 but the project ceiling is 2.
    assert_eq!(count_spawns(&fx), 2);
}

#[test]
fn queued_spawn_recorded_and_slot_consumed() {
    // A Spawned event (queued or running alike) records the binding; the task is
    // now in-flight so a subsequent scan does not double-dispatch it.
    let mut p = project(ProjectPhase::Running, vec![task("t1", TaskState::Todo, 0, &[])]);
    step(&mut p, Input::Host(HostEvent::Tick), 1000, 4);
    step(
        &mut p,
        Input::Host(HostEvent::Spawned {
            task: TaskId("t1".into()),
            agent_id: 77,
            spawned_at_ms: 1000,
        }),
        1000,
        4,
    );
    match &find_task(&p, "t1").state {
        TaskState::OnProgress { binding, .. } => assert_eq!(binding.ext_agent_id, 77),
        s => panic!("expected OnProgress, got {:?}", s),
    }
    // Next tick: no second spawn.
    let fx = step(&mut p, Input::Host(HostEvent::Tick), 1500, 4);
    assert_eq!(count_spawns(&fx), 0);
}

// ---------------------------------------------------------------------------
// Runtime ceiling (5.2.4)
// ---------------------------------------------------------------------------

#[test]
fn runtime_ceiling_kills_and_requeues_regardless_of_liveness() {
    let mut p = project(
        ProjectPhase::Running,
        vec![task(
            "t1",
            TaskState::OnProgress {
                binding: worker_binding(42, 0),
                attempt: 1,
            },
            0,
            &[],
        )],
    );
    // Reconcile at a time far past the 20-minute ceiling.
    let now = p.config.worker_max_runtime_ms + 5_000;
    let fx = step(&mut p, Input::Host(HostEvent::Reconcile), now, 0);
    assert!(fx.iter().any(|e| matches!(e, Effect::Kill { ext_agent_id: 42 })));
    // Re-queued to Todo with attempt++.
    assert!(matches!(find_task(&p, "t1").state, TaskState::Todo));
    assert_eq!(next_attempt(find_task(&p, "t1")), 2);
}

#[test]
fn runtime_ceiling_ignores_fresh_and_provisional_bindings() {
    let mut p = project(
        ProjectPhase::Running,
        vec![
            // Fresh real binding, well within the ceiling.
            task(
                "fresh",
                TaskState::OnProgress {
                    binding: worker_binding(42, 1_000_000),
                    attempt: 1,
                },
                0,
                &[],
            ),
            // Provisional binding (id 0) must never be killed.
            task(
                "prov",
                TaskState::OnProgress {
                    binding: worker_binding(0, 0),
                    attempt: 1,
                },
                0,
                &[],
            ),
        ],
    );
    let fx = step(&mut p, Input::Host(HostEvent::Reconcile), 1_100_000, 0);
    assert!(!fx.iter().any(|e| matches!(e, Effect::Kill { .. })));
}

// ---------------------------------------------------------------------------
// Comment receipt discipline (5.3)
// ---------------------------------------------------------------------------

#[test]
fn comment_delivered_on_spawn_and_read_only_on_ack() {
    let mut p = project(ProjectPhase::Running, vec![task("t1", TaskState::Todo, 0, &[])]);
    // Add a comment while the task is still Todo.
    step(
        &mut p,
        Input::Command(Command::AddComment {
            task: TaskId("t1".into()),
            author: CommentAuthor::User,
            text: "watch the edge case".into(),
        }),
        900,
        0, // cap 0: no dispatch yet, so the comment stays Pending
    );
    assert!(matches!(find_task(&p, "t1").comments[0].receipt, Receipt::Pending));

    // Dispatch folds the comment -> Delivered.
    step(&mut p, Input::Host(HostEvent::Tick), 1000, 4);
    assert!(matches!(
        find_task(&p, "t1").comments[0].receipt,
        Receipt::Delivered { .. }
    ));

    // Worker report ACKs it -> Read.
    let cid = find_task(&p, "t1").comments[0].id.0;
    step(
        &mut p,
        Input::Host(HostEvent::Spawned {
            task: TaskId("t1".into()),
            agent_id: 5,
            spawned_at_ms: 1000,
        }),
        1000,
        4,
    );
    let report = format!(
        "did it\nOFFICE-REPORT\nstatus: complete\nsummary: built it\nack-comments: c{}\n",
        cid
    );
    step(&mut p, Input::Host(HostEvent::Result { agent_id: 5, text: report }), 2000, 4);
    assert!(matches!(find_task(&p, "t1").comments[0].receipt, Receipt::Read { .. }));
}

#[test]
fn ack_never_flips_a_never_delivered_comment() {
    // A comment added mid-run (never folded) is Pending; an ack token for it must NOT
    // flip it to Read — the office never claims a read it did not deliver.
    let mut t = task(
        "t1",
        TaskState::OnProgress {
            binding: worker_binding(9, 0),
            attempt: 1,
        },
        0,
        &[],
    );
    t.comments.push(Comment {
        id: CommentId(1),
        author: CommentAuthor::User,
        text: "late note".into(),
        created_ms: 500,
        receipt: Receipt::Pending,
    });
    let mut p = project(ProjectPhase::Running, vec![t]);

    let report = "did it\nOFFICE-REPORT\nstatus: complete\nsummary: s\nack-comments: c1\n";
    step(&mut p, Input::Host(HostEvent::Result { agent_id: 9, text: report.into() }), 2000, 0);
    assert!(matches!(find_task(&p, "t1").comments[0].receipt, Receipt::Pending));
}

#[test]
fn pending_comment_stays_pending_through_first_try_done() {
    // Comment added mid-run, worker passes on the first try; task reaches Done with
    // the comment still Pending (task completion never flips a receipt).
    let mut p = project(
        ProjectPhase::Running,
        vec![task(
            "t1",
            TaskState::OnProgress {
                binding: worker_binding(1, 0),
                attempt: 1,
            },
            0,
            &[],
        )],
    );
    step(
        &mut p,
        Input::Command(Command::AddComment {
            task: TaskId("t1".into()),
            author: CommentAuthor::User,
            text: "note".into(),
        }),
        100,
        0,
    );
    // Worker done (report has no ack) -> Review.
    step(&mut p, Input::Host(HostEvent::Result { agent_id: 1, text: REPORT_OK.into() }), 200, 0);
    assert!(matches!(find_task(&p, "t1").comments[0].receipt, Receipt::Pending));
    // Reviewer spawns, passes -> Done. cap 0 keeps us from spawning the reviewer here,
    // so drive the reviewer path with capacity.
    step(&mut p, Input::Host(HostEvent::Tick), 300, 4); // spawn reviewer
    // reviewer got a provisional binding; give it a real id then pass it.
    step(
        &mut p,
        Input::Host(HostEvent::Spawned {
            task: TaskId("t1".into()),
            agent_id: 2,
            spawned_at_ms: 300,
        }),
        300,
        4,
    );
    step(&mut p, Input::Host(HostEvent::Result { agent_id: 2, text: REVIEW_PASS.into() }), 400, 4);
    assert!(matches!(find_task(&p, "t1").state, TaskState::Done { .. }));
    assert!(matches!(find_task(&p, "t1").comments[0].receipt, Receipt::Pending));
}

// ---------------------------------------------------------------------------
// Mid-run comment injection (feature 4): a comment added to a task with a LIVE binding is
// pushed to the running agent via `agents.send` (Effect::InjectComment); the receipt flips
// Pending -> Delivered ONLY when the driver confirms with HostEvent::CommentDelivered.
// ---------------------------------------------------------------------------

#[test]
fn live_worker_comment_emits_inject_and_delivers_on_success_event() {
    let mut p = project(
        ProjectPhase::Running,
        vec![task(
            "t1",
            TaskState::OnProgress { binding: worker_binding(7, 0), attempt: 1 },
            0,
            &[],
        )],
    );
    let fx = step(
        &mut p,
        Input::Command(Command::AddComment {
            task: TaskId("t1".into()),
            author: CommentAuthor::User,
            text: "watch the race".into(),
        }),
        100,
        0, // cap 0: no dispatch churn, isolate the injection behavior
    );
    let cid = find_task(&p, "t1").comments[0].id;
    // The comment is pushed to the live worker (id 7), framed with its id + ack instruction.
    assert!(fx.iter().any(|e| matches!(
        e,
        Effect::InjectComment { ext_agent_id: 7, comment_id, text }
            if *comment_id == cid
                && text.contains("watch the race")
                && text.contains(&format!("c{}", cid.0))
                && text.contains("OFFICE-REPORT")
    )));
    // Emission alone does not deliver: the receipt waits for the driver's success event.
    assert!(matches!(find_task(&p, "t1").comments[0].receipt, Receipt::Pending));

    // Driver reports `agents.send` succeeded -> Delivered.
    step(
        &mut p,
        Input::Host(HostEvent::CommentDelivered { task: TaskId("t1".into()), comment_id: cid }),
        200,
        0,
    );
    assert!(matches!(find_task(&p, "t1").comments[0].receipt, Receipt::Delivered { .. }));
}

#[test]
fn live_reviewer_comment_emits_inject_to_reviewer_agent() {
    // A `Review` state with a spawned reviewer binding is also a live target.
    let mut p = project(
        ProjectPhase::Running,
        vec![task(
            "t1",
            TaskState::Review { binding: Some(reviewer_binding(9, 0)), attempt: 1 },
            0,
            &[],
        )],
    );
    let fx = step(
        &mut p,
        Input::Command(Command::AddComment {
            task: TaskId("t1".into()),
            author: CommentAuthor::User,
            text: "double-check the edge case".into(),
        }),
        100,
        0,
    );
    assert!(fx.iter().any(|e| matches!(e, Effect::InjectComment { ext_agent_id: 9, .. })));
}

#[test]
fn live_comment_without_delivery_event_stays_pending() {
    let mut p = project(
        ProjectPhase::Running,
        vec![task(
            "t1",
            TaskState::OnProgress { binding: worker_binding(7, 0), attempt: 1 },
            0,
            &[],
        )],
    );
    let fx = step(
        &mut p,
        Input::Command(Command::AddComment {
            task: TaskId("t1".into()),
            author: CommentAuthor::User,
            text: "note".into(),
        }),
        100,
        0,
    );
    assert!(fx.iter().any(|e| matches!(e, Effect::InjectComment { ext_agent_id: 7, .. })));
    // No CommentDelivered fed (an `agents.send` error path) -> stays Pending.
    assert!(matches!(find_task(&p, "t1").comments[0].receipt, Receipt::Pending));
    // A CommentDelivered for a DIFFERENT (unknown) comment id is a no-op.
    step(
        &mut p,
        Input::Host(HostEvent::CommentDelivered {
            task: TaskId("t1".into()),
            comment_id: CommentId(999),
        }),
        150,
        0,
    );
    assert!(matches!(find_task(&p, "t1").comments[0].receipt, Receipt::Pending));
}

#[test]
fn comment_without_live_binding_emits_no_inject() {
    // Todo (no binding), Review{None} (no reviewer yet), and a PROVISIONAL worker binding
    // (id 0, spawn not yet acked) all lack a reachable agent: AddComment must not emit an
    // InjectComment, and the comment simply waits Pending for the spawn-boundary fold.
    for state in [
        TaskState::Todo,
        TaskState::Review { binding: None, attempt: 1 },
        TaskState::OnProgress { binding: worker_binding(0, 0), attempt: 1 },
    ] {
        let mut p = project(ProjectPhase::Running, vec![task("t1", state, 0, &[])]);
        let fx = step(
            &mut p,
            Input::Command(Command::AddComment {
                task: TaskId("t1".into()),
                author: CommentAuthor::User,
                text: "n".into(),
            }),
            100,
            0,
        );
        assert!(!fx.iter().any(|e| matches!(e, Effect::InjectComment { .. })));
        assert!(matches!(find_task(&p, "t1").comments[0].receipt, Receipt::Pending));
    }
}

#[test]
fn injected_comment_folds_at_next_spawn_when_delivery_never_confirmed() {
    // Live-binding comment emits InjectComment, but if the driver never confirms delivery the
    // receipt stays Pending — and the existing spawn-boundary fold still delivers it when the
    // task bounces and re-dispatches a worker.
    let mut p = project(
        ProjectPhase::Running,
        vec![task(
            "t1",
            TaskState::OnProgress { binding: worker_binding(7, 0), attempt: 1 },
            0,
            &[],
        )],
    );
    step(
        &mut p,
        Input::Command(Command::AddComment {
            task: TaskId("t1".into()),
            author: CommentAuthor::User,
            text: "note".into(),
        }),
        100,
        0,
    );
    assert!(matches!(find_task(&p, "t1").comments[0].receipt, Receipt::Pending));

    // Worker finishes -> Review (still Pending; no delivery event, no ack).
    step(&mut p, Input::Host(HostEvent::Result { agent_id: 7, text: REPORT_OK.into() }), 200, 0);
    assert!(matches!(find_task(&p, "t1").comments[0].receipt, Receipt::Pending));

    // Reviewer spawns, then FAILS -> the task bounces back to Todo and the same step
    // re-dispatches a worker, whose spawn-boundary fold flips the still-Pending comment.
    step(&mut p, Input::Host(HostEvent::Tick), 300, 4); // spawn reviewer (provisional)
    step(
        &mut p,
        Input::Host(HostEvent::Spawned { task: TaskId("t1".into()), agent_id: 8, spawned_at_ms: 300 }),
        300,
        4,
    );
    step(&mut p, Input::Host(HostEvent::Result { agent_id: 8, text: REVIEW_FAIL.into() }), 400, 4);
    assert!(matches!(find_task(&p, "t1").comments[0].receipt, Receipt::Delivered { .. }));
}

// ---------------------------------------------------------------------------
// Desk layout (ARCHITECTURE.md 7.1): flat, human-readable, obviously-marked
// ---------------------------------------------------------------------------

#[test]
fn dispatch_desk_dir_is_flat_project_slug_over_task_slug() {
    // Real dispatch mints hierarchical TaskIds `<project>/<epic-slug>/<story-slug>/<task-slug>`
    // (office::apply_breakdown). The desk dir must collapse that to the single flat,
    // obviously-marked `desks/<project-slug>/<task-slug>--koma-workflow-desk` layout locked by
    // ARCHITECTURE.md 7.1 -- never a nested epic/story nested directory tree.
    let mut p = project(
        ProjectPhase::Running,
        vec![task(
            "shop-crawler/e1-ingest/s2-parser/t4-retry-logic",
            TaskState::Todo,
            0,
            &[],
        )],
    );
    p.id = ProjectId("shop-crawler".to_string());

    let fx = step(&mut p, Input::Host(HostEvent::Tick), 1000, 4);
    let dir = fx
        .iter()
        .find_map(|e| match e {
            Effect::EnsureDesk { dir, .. } => Some(dir.clone()),
            _ => None,
        })
        .expect("EnsureDesk effect");

    assert_eq!(
        dir,
        PathBuf::from("/ws/koma-workflow/desks/shop-crawler/t4-retry-logic--koma-workflow-desk")
    );
}

// ---------------------------------------------------------------------------
// Full lifecycle: exact effect sequence
// ---------------------------------------------------------------------------

#[test]
fn full_task_lifecycle_effect_sequence() {
    let mut p = project(ProjectPhase::Running, vec![task("t1", TaskState::Todo, 0, &[])]);

    // 1. Tick -> EnsureDesk + Spawn(worker) + Persist + PanelPush.
    let fx = step(&mut p, Input::Host(HostEvent::Tick), 1000, 4);
    assert!(matches!(fx[0], Effect::EnsureDesk { .. }));
    // A worker spawn now carries the task's persona id (office-worker-<name>), stably
    // hashed from the task id — deterministic, so it equals worker_agent_id("t1").
    assert!(matches!(&fx[1], Effect::Spawn { agent, .. } if *agent == crate::persona::worker_agent_id("t1")));
    assert!(matches!(fx[2], Effect::Persist));
    assert!(matches!(fx[3], Effect::PanelPush { .. }));
    assert_eq!(fx.len(), 4);

    // 2. Spawned -> record id -> Persist + PanelPush.
    let fx = step(
        &mut p,
        Input::Host(HostEvent::Spawned {
            task: TaskId("t1".into()),
            agent_id: 10,
            spawned_at_ms: 1000,
        }),
        1000,
        4,
    );
    assert_eq!(fx, vec![Effect::Persist, Effect::PanelPush { snapshot: true }]);

    // 3. AgentsDone(done) -> FetchResult only (no state change).
    let fx = step(
        &mut p,
        Input::Host(HostEvent::AgentsDone {
            agent_id: 10,
            status: "done".into(),
            error: None,
        }),
        1500,
        4,
    );
    assert_eq!(fx, vec![Effect::FetchResult { ext_agent_id: 10 }]);

    // 4. Result(complete) -> Review -> reviewer spawn + Persist + PanelPush.
    let fx = step(&mut p, Input::Host(HostEvent::Result { agent_id: 10, text: REPORT_OK.into() }), 2000, 4);
    assert_eq!(spawn_agents(&fx), vec!["office-reviewer"]);
    assert!(matches!(
        find_task(&p, "t1").state,
        TaskState::Review { binding: Some(_), .. }
    ));

    // 5. Reviewer spawned + done + pass -> Done, project Done.
    step(
        &mut p,
        Input::Host(HostEvent::Spawned {
            task: TaskId("t1".into()),
            agent_id: 11,
            spawned_at_ms: 2000,
        }),
        2000,
        4,
    );
    let fx = step(
        &mut p,
        Input::Host(HostEvent::AgentsDone {
            agent_id: 11,
            status: "done".into(),
            error: None,
        }),
        2500,
        4,
    );
    assert_eq!(fx, vec![Effect::FetchResult { ext_agent_id: 11 }]);
    step(&mut p, Input::Host(HostEvent::Result { agent_id: 11, text: REVIEW_PASS.into() }), 3000, 4);
    assert!(matches!(find_task(&p, "t1").state, TaskState::Done { at_ms: 3000 }));
    assert!(matches!(p.phase, ProjectPhase::Done { .. }));
}

// ---------------------------------------------------------------------------
// Bounce / park path
// ---------------------------------------------------------------------------

#[test]
fn review_fail_within_budget_requeues_with_notes() {
    let mut p = project(
        ProjectPhase::Running,
        vec![task(
            "t1",
            TaskState::Review {
                binding: Some(reviewer_binding(3, 0)),
                attempt: 1,
            },
            0,
            &[],
        )],
    );
    p.config.bounce_budget = 3;
    // cap 0 so we observe the Todo re-queue before re-dispatch.
    step(&mut p, Input::Host(HostEvent::Result { agent_id: 3, text: REVIEW_FAIL.into() }), 1000, 0);
    let t = find_task(&p, "t1");
    assert!(matches!(t.state, TaskState::Todo));
    assert_eq!(t.bounces, 1);
    assert_eq!(next_attempt(t), 2);
    assert_eq!(t.last_review.as_deref(), Some("missing tests"));
}

#[test]
fn review_fail_over_budget_escalates_and_parks() {
    let mut t = task(
        "t1",
        TaskState::Review {
            binding: Some(reviewer_binding(3, 0)),
            attempt: 2,
        },
        0,
        &[],
    );
    t.bounces = 3; // already at budget
    let mut p = project(ProjectPhase::Running, vec![t]);
    p.config.bounce_budget = 3;

    let fx = step(&mut p, Input::Host(HostEvent::Result { agent_id: 3, text: REVIEW_FAIL.into() }), 1000, 0);
    assert!(fx.iter().any(|e| matches!(e, Effect::QueueChatPrompt { .. })));
    assert!(matches!(
        find_task(&p, "t1").state,
        TaskState::Parked {
            reason: ParkReason::ReviewBounceBudget,
            ..
        }
    ));
}

#[test]
fn worker_blocked_report_parks_task() {
    let mut p = project(
        ProjectPhase::Running,
        vec![
            task(
                "t1",
                TaskState::OnProgress {
                    binding: worker_binding(4, 0),
                    attempt: 1,
                },
                0,
                &[],
            ),
            // A second unrelated task keeps the line un-stuck (no halt) so we isolate the park.
            task("t2", TaskState::Todo, 0, &[]),
        ],
    );
    let blocked = "OFFICE-REPORT\nstatus: blocked\nblocked-reason: need a decision\n";
    step(&mut p, Input::Host(HostEvent::Result { agent_id: 4, text: blocked.into() }), 1000, 0);
    assert!(matches!(
        find_task(&p, "t1").state,
        TaskState::Parked {
            reason: ParkReason::WorkerBlocked(_),
            ..
        }
    ));
    assert!(matches!(p.phase, ProjectPhase::Running));
}

// ---------------------------------------------------------------------------
// Worker error / spawn failure
// ---------------------------------------------------------------------------

#[test]
fn worker_error_requeues_with_attempt_increment() {
    let mut p = project(
        ProjectPhase::Running,
        vec![task(
            "t1",
            TaskState::OnProgress {
                binding: worker_binding(6, 0),
                attempt: 1,
            },
            0,
            &[],
        )],
    );
    step(
        &mut p,
        Input::Host(HostEvent::AgentsDone {
            agent_id: 6,
            status: "error".into(),
            error: None,
        }),
        1000,
        0, // cap 0: observe Todo before re-dispatch
    );
    let t = find_task(&p, "t1");
    assert!(matches!(t.state, TaskState::Todo));
    assert_eq!(next_attempt(t), 2);
}

#[test]
fn three_spawn_failures_park_the_task() {
    let mut p = project(ProjectPhase::Running, vec![task("t1", TaskState::Todo, 0, &[])]);
    // Tick dispatches worker 1.
    step(&mut p, Input::Host(HostEvent::Tick), 1000, 4);
    for i in 0..3 {
        let fx = step(
            &mut p,
            Input::Host(HostEvent::SpawnFailed {
                task: TaskId("t1".into()),
                reason: "session not live".into(),
            }),
            1000 + i,
            4, // re-dispatch after each failure (except the parking one)
        );
        // On the third failure the task parks instead of re-dispatching.
        if i < 2 {
            assert!(count_spawns(&fx) >= 1, "should re-dispatch after failure {}", i);
        }
    }
    assert!(matches!(
        find_task(&p, "t1").state,
        TaskState::Parked {
            reason: ParkReason::SpawnFailed(_),
            ..
        }
    ));
}

// ---------------------------------------------------------------------------
// Halt detection
// ---------------------------------------------------------------------------

#[test]
fn park_that_stucks_the_line_halts_the_project() {
    // t1 is running; t2 depends on t1. When t1's worker reports blocked -> Parked,
    // the only unfinished task (t2) is transitively blocked by a Parked task -> Halt.
    let mut p = project(
        ProjectPhase::Running,
        vec![
            task(
                "t1",
                TaskState::OnProgress {
                    binding: worker_binding(4, 0),
                    attempt: 1,
                },
                0,
                &[],
            ),
            task("t2", TaskState::Todo, 0, &["t1"]),
        ],
    );
    let blocked = "OFFICE-REPORT\nstatus: blocked\nblocked-reason: need a decision\n";
    let fx = step(&mut p, Input::Host(HostEvent::Result { agent_id: 4, text: blocked.into() }), 1000, 0);
    assert!(matches!(p.phase, ProjectPhase::Halted { .. }));
    assert!(fx.iter().any(|e| matches!(e, Effect::QueueChatPrompt { .. })));
}

#[test]
fn draining_last_task_after_a_park_still_halts() {
    // t1 and t2 are independent (no blocked_by between them), both mid-review.
    // t1's reviewer fails over budget first -> Parked, but t2 is still Review
    // (running) so check_halt at that moment sees the line as not-yet-stuck.
    // t2's reviewer later passes -> Done. That drain must re-check halt: with
    // t1 Parked and nothing left running, the line is now stuck and must Halt.
    let mut t1 = task(
        "t1",
        TaskState::Review {
            binding: Some(reviewer_binding(3, 0)),
            attempt: 2,
        },
        0,
        &[],
    );
    t1.bounces = 3; // already at budget
    let t2 = task(
        "t2",
        TaskState::Review {
            binding: Some(reviewer_binding(5, 0)),
            attempt: 1,
        },
        0,
        &[],
    );
    let mut p = project(ProjectPhase::Running, vec![t1, t2]);
    p.config.bounce_budget = 3;

    // t1's reviewer fails over budget -> park t1. t2 is still running, so no halt yet.
    step(&mut p, Input::Host(HostEvent::Result { agent_id: 3, text: REVIEW_FAIL.into() }), 1000, 0);
    assert!(matches!(
        find_task(&p, "t1").state,
        TaskState::Parked {
            reason: ParkReason::ReviewBounceBudget,
            ..
        }
    ));
    assert!(matches!(p.phase, ProjectPhase::Running));

    // t2's reviewer passes -> Done. Now t1 (Parked) is the only unfinished task
    // and nothing is running: the line must be recognized as stuck and Halted.
    let fx = step(&mut p, Input::Host(HostEvent::Result { agent_id: 5, text: REVIEW_PASS.into() }), 2000, 0);
    assert!(matches!(find_task(&p, "t2").state, TaskState::Done { .. }));
    assert!(matches!(p.phase, ProjectPhase::Halted { .. }));
    assert!(fx.iter().any(|e| matches!(e, Effect::QueueChatPrompt { .. })));
}

#[test]
fn a_ready_task_keeps_the_line_unstuck() {
    // Same as above but t3 is independently ready -> no halt after t1 parks.
    let mut p = project(
        ProjectPhase::Running,
        vec![
            task(
                "t1",
                TaskState::OnProgress {
                    binding: worker_binding(4, 0),
                    attempt: 1,
                },
                0,
                &[],
            ),
            task("t2", TaskState::Todo, 0, &["t1"]),
            task("t3", TaskState::Todo, 0, &[]),
        ],
    );
    let blocked = "OFFICE-REPORT\nstatus: blocked\nblocked-reason: x\n";
    step(&mut p, Input::Host(HostEvent::Result { agent_id: 4, text: blocked.into() }), 1000, 0);
    assert!(matches!(p.phase, ProjectPhase::Running));
}

// ---------------------------------------------------------------------------
// Interrupt / resume
// ---------------------------------------------------------------------------

#[test]
fn hard_interrupt_kills_and_normalizes() {
    let mut p = project(
        ProjectPhase::Running,
        vec![
            task(
                "t1",
                TaskState::OnProgress {
                    binding: worker_binding(5, 0),
                    attempt: 2,
                },
                0,
                &[],
            ),
            task(
                "t2",
                TaskState::Review {
                    binding: Some(reviewer_binding(6, 0)),
                    attempt: 1,
                },
                0,
                &[],
            ),
        ],
    );
    let fx = step(&mut p, Input::Command(Command::Interrupt { hard: true }), 1000, 4);
    assert!(matches!(p.phase, ProjectPhase::Interrupted));
    assert!(fx.iter().any(|e| matches!(e, Effect::Kill { ext_agent_id: 5 })));
    assert!(fx.iter().any(|e| matches!(e, Effect::Kill { ext_agent_id: 6 })));
    // Worker normalized to Todo (attempt preserved), reviewer to Review{None}.
    assert!(matches!(find_task(&p, "t1").state, TaskState::Todo));
    assert_eq!(next_attempt(find_task(&p, "t1")), 2);
    assert!(matches!(
        find_task(&p, "t2").state,
        TaskState::Review { binding: None, attempt: 1 }
    ));
}

#[test]
fn soft_interrupt_drains_without_killing() {
    let mut p = project(
        ProjectPhase::Running,
        vec![task(
            "t1",
            TaskState::OnProgress {
                binding: worker_binding(5, 0),
                attempt: 1,
            },
            0,
            &[],
        )],
    );
    let fx = step(&mut p, Input::Command(Command::Interrupt { hard: false }), 1000, 4);
    assert!(matches!(p.phase, ProjectPhase::Interrupted));
    assert!(!fx.iter().any(|e| matches!(e, Effect::Kill { .. })));
    // In-flight worker left untouched to finish.
    assert!(matches!(find_task(&p, "t1").state, TaskState::OnProgress { .. }));
}

#[test]
fn resume_re_dispatches() {
    let mut p = project(ProjectPhase::Interrupted, vec![task("t1", TaskState::Todo, 0, &[])]);
    let fx = step(&mut p, Input::Command(Command::Resume), 1000, 4);
    assert!(matches!(p.phase, ProjectPhase::Running));
    assert_eq!(spawn_agents(&fx), vec![crate::persona::worker_agent_id("t1").as_str()]);
}

#[test]
fn unpark_returns_task_to_todo_preserving_attempt() {
    let mut p = project(
        ProjectPhase::Interrupted, // not Running: no auto re-dispatch
        vec![task(
            "t1",
            TaskState::Parked {
                reason: ParkReason::ReviewBounceBudget,
                attempt: 3,
            },
            0,
            &[],
        )],
    );
    step(&mut p, Input::Command(Command::Unpark { task: TaskId("t1".into()) }), 1000, 4);
    let t = find_task(&p, "t1");
    assert!(matches!(t.state, TaskState::Todo));
    assert_eq!(next_attempt(t), 3);
}

// ---------------------------------------------------------------------------
// config_set (10.2 panel op; ProjectConfig direct edit)
// ---------------------------------------------------------------------------

#[test]
fn config_set_applies_only_the_provided_fields() {
    let mut p = project(ProjectPhase::Interrupted, vec![]);
    assert!(!p.config.keep_desks, "starts false, matching ProjectConfig::default_config");

    let fx = step(
        &mut p,
        Input::Command(Command::ConfigSet {
            max_workers: Some(3),
            bounce_budget: None,
            worker_model: None,
            reviewer_model: Some("claude-opus".to_string()),
            keep_desks: Some(true),
            crd_pass_grade: Some(90),
            assumption_check: Some(false),
            assumption_mode: Some("ask".to_string()),
            research_mode: None,
            drafter_model: None,
        }),
        1000,
        4,
    );

    assert_eq!(p.config.max_workers, 3, "provided field is applied");
    assert_eq!(p.config.bounce_budget, 3, "absent field keeps ProjectConfig::default_config's value");
    assert_eq!(p.config.worker_model, None, "absent field is left untouched, not cleared");
    assert_eq!(p.config.reviewer_model, Some("claude-opus".to_string()));
    assert!(p.config.keep_desks, "keepDesks parses through into ProjectConfig.keep_desks");
    // 6.2c config fields round-trip through config_set exactly like the rest.
    assert_eq!(p.config.crd_pass_grade, 90, "crdPassGrade parses through into ProjectConfig");
    assert!(!p.config.assumption_check, "assumptionCheck=false disables the safeguard gate");
    assert_eq!(p.config.assumption_mode, "ask", "assumptionMode parses through into ProjectConfig");
    assert!(fx.iter().any(|e| matches!(e, Effect::Persist)), "a mutation always persists");
}

#[test]
fn config_set_crd_pass_grade_is_clamped_to_100() {
    let mut p = project(ProjectPhase::Interrupted, vec![]);
    step(
        &mut p,
        Input::Command(Command::ConfigSet {
            max_workers: None,
            bounce_budget: None,
            worker_model: None,
            reviewer_model: None,
            keep_desks: None,
            crd_pass_grade: Some(250),
            assumption_check: None,
            assumption_mode: None,
            research_mode: None,
            drafter_model: None,
        }),
        1000,
        4,
    );
    assert_eq!(p.config.crd_pass_grade, 100, "a rubric grade over 100 is clamped");
}

#[test]
fn config_set_max_workers_is_clamped_within_1_to_4() {
    let mut p = project(ProjectPhase::Interrupted, vec![]);
    step(
        &mut p,
        Input::Command(Command::ConfigSet {
            max_workers: Some(99),
            bounce_budget: None,
            worker_model: None,
            reviewer_model: None,
            keep_desks: None,
            crd_pass_grade: None,
            assumption_check: None,
            assumption_mode: None,
            research_mode: None,
            drafter_model: None,
        }),
        1000,
        4,
    );
    assert_eq!(p.config.max_workers, 4);

    step(
        &mut p,
        Input::Command(Command::ConfigSet {
            max_workers: Some(0),
            bounce_budget: None,
            worker_model: None,
            reviewer_model: None,
            keep_desks: None,
            crd_pass_grade: None,
            assumption_check: None,
            assumption_mode: None,
            research_mode: None,
            drafter_model: None,
        }),
        1000,
        4,
    );
    assert_eq!(p.config.max_workers, 1);
}

#[test]
fn config_set_with_no_fields_is_a_no_op_and_does_not_mark_dirty() {
    let mut p = project(ProjectPhase::Interrupted, vec![]);
    let before = p.config.clone();
    let fx = step(
        &mut p,
        Input::Command(Command::ConfigSet {
            max_workers: None,
            bounce_budget: None,
            worker_model: None,
            reviewer_model: None,
            keep_desks: None,
            crd_pass_grade: None,
            assumption_check: None,
            assumption_mode: None,
            research_mode: None,
            drafter_model: None,
        }),
        1000,
        4,
    );
    assert_eq!(p.config, before);
    assert!(!fx.iter().any(|e| matches!(e, Effect::Persist)), "no fields provided -> no dirty -> no persist");
}

// ---------------------------------------------------------------------------
// Determinism
// ---------------------------------------------------------------------------

// ---------------------------------------------------------------------------
// Breakdown timeout -> compact retry ladder (6.3.2)
// ---------------------------------------------------------------------------

const BREAKDOWN_JSON_OK: &str = r#"{"epics":[{"slug":"e1","title":"Epic","intent":"i","stories":[{"slug":"s1","title":"Story","intent":"i","tasks":[{"slug":"t1","title":"Task","description":"d","acceptance":["ok"],"priority":0,"blocked_by":[]}]}]}]}"#;

fn invoke_result(purpose: InvokePurpose, outcome: Result<String, String>) -> Input {
    Input::Command(Command::InvokeResult { purpose, outcome })
}

fn invoke_effects(fx: &[Effect]) -> Vec<&Effect> {
    fx.iter().filter(|e| matches!(e, Effect::InvokeModel { .. })).collect()
}

/// Assert exactly one `InvokeModel` effect was emitted and return its purpose.
fn sole_invoke_purpose(fx: &[Effect]) -> InvokePurpose {
    let invokes = invoke_effects(fx);
    assert_eq!(invokes.len(), 1, "expected exactly one InvokeModel effect");
    match invokes[0] {
        Effect::InvokeModel { purpose, .. } => *purpose,
        other => panic!("expected InvokeModel, got {other:?}"),
    }
}

#[test]
fn breakdown_timeout_falls_back_to_one_compact_invoke() {
    // The kernel only ever sees this Err after the driver's own pool-level retry has
    // already run and also timed out (driver.rs on_invoke_done) — see the doc comment on
    // handle_breakdown_result. From the kernel's perspective it is simply: Breakdown Err
    // "timed out" -> exactly one BreakdownCompact invoke, carrying the compact contract.
    let mut p = project(ProjectPhase::Drafting, vec![]);
    let fx = step(
        &mut p,
        invoke_result(InvokePurpose::Breakdown, Err("model call timed out".to_string())),
        1000,
        4,
    );

    let invokes = invoke_effects(&fx);
    assert_eq!(invokes.len(), 1, "exactly one compact breakdown invoke is queued");
    match invokes[0] {
        Effect::InvokeModel { purpose, prompt, .. } => {
            assert_eq!(*purpose, InvokePurpose::BreakdownCompact);
            assert!(prompt.contains("6 tasks"), "compact contract present in the prompt: {prompt}");
            assert!(prompt.contains("COMPACT MODE"), "compact contract present in the prompt: {prompt}");
        }
        other => panic!("expected InvokeModel, got {other:?}"),
    }
    // Nothing surfaced to the user yet — the compact attempt has not resolved.
    assert!(p.outbox.is_empty());
}

#[test]
fn breakdown_non_timeout_error_surfaces_immediately_no_compact_fallback() {
    let mut p = project(ProjectPhase::Drafting, vec![]);
    let fx = step(
        &mut p,
        invoke_result(InvokePurpose::Breakdown, Err("some other failure".to_string())),
        1000,
        4,
    );

    assert!(invoke_effects(&fx).is_empty(), "a non-timeout error never falls back to compact");
    assert!(p
        .outbox
        .iter()
        .any(|n| n.text.contains("office breakdown call failed: some other failure")));
}

#[test]
fn breakdown_compact_success_lands_tasks_and_ready_notice() {
    let mut p = project(ProjectPhase::Drafting, vec![]);
    // design-speedup item 8: a breakdown result in Drafting is stashed and applied by the JOIN once
    // the TRD+CRD gate has cleared; pin gate_cleared so this isolates the breakdown-landing itself.
    p.gate_cleared = true;
    let fx = step(
        &mut p,
        invoke_result(InvokePurpose::BreakdownCompact, Ok(BREAKDOWN_JSON_OK.to_string())),
        1000,
        4,
    );

    assert!(invoke_effects(&fx).is_empty());
    assert_eq!(p.phase, ProjectPhase::Ready, "Drafting -> Ready on a landed compact breakdown");
    assert_eq!(p.tasks.len(), 1);
    assert_eq!(p.epics.len(), 1);
    assert!(
        p.outbox.iter().any(|n| n.text.contains("board is ready")),
        "the usual board-ready notice fires for a compact landing too"
    );
}

#[test]
fn breakdown_compact_timeout_surfaces_actionable_notice() {
    let mut p = project(ProjectPhase::Drafting, vec![]);
    let fx = step(
        &mut p,
        invoke_result(InvokePurpose::BreakdownCompact, Err("model call timed out".to_string())),
        1000,
        4,
    );

    assert!(invoke_effects(&fx).is_empty(), "no further retry after the compact attempt");
    assert!(p.epics.is_empty(), "nothing landed on a compact failure");
    let notice = p.outbox.iter().find(|n| n.text.contains("office breakdown call failed"));
    let notice = notice.expect("an actionable notice was queued");
    assert!(notice.text.contains("workflow_breakdown"), "notice: {}", notice.text);
    assert!(notice.text.contains("faster model"), "notice: {}", notice.text);
}

#[test]
fn breakdown_compact_parse_failure_also_surfaces_actionable_notice() {
    let mut p = project(ProjectPhase::Drafting, vec![]);
    let fx = step(
        &mut p,
        invoke_result(InvokePurpose::BreakdownCompact, Ok("not json".to_string())),
        1000,
        4,
    );

    assert!(invoke_effects(&fx).is_empty(), "compact never re-asks — one shot only");
    let notice = p.outbox.iter().find(|n| n.text.contains("compact retry"));
    let notice = notice.expect("an actionable notice was queued");
    assert!(notice.text.contains("workflow_breakdown"), "notice: {}", notice.text);
}

#[test]
fn breakdown_parse_failure_reasks_once_then_surfaces_unchanged() {
    // Locks in the pre-existing re-ask ladder, untouched by the compact timeout fallback.
    let mut p = project(ProjectPhase::Drafting, vec![]);
    let fx = step(
        &mut p,
        invoke_result(InvokePurpose::Breakdown, Ok("not json".to_string())),
        1000,
        4,
    );
    let invokes = invoke_effects(&fx);
    assert_eq!(invokes.len(), 1);
    match invokes[0] {
        Effect::InvokeModel { purpose, .. } => assert_eq!(*purpose, InvokePurpose::BreakdownReask),
        other => panic!("expected InvokeModel, got {other:?}"),
    }

    let fx2 = step(
        &mut p,
        invoke_result(InvokePurpose::BreakdownReask, Ok("still not json".to_string())),
        2000,
        4,
    );
    assert!(invoke_effects(&fx2).is_empty(), "the re-ask ladder stops after one retry");
    assert!(p.outbox.iter().any(|n| n.text.contains("rejected twice")));
    assert!(p.epics.is_empty());
}

#[test]
fn breakdown_reask_success_lands_tasks_unchanged() {
    let mut p = project(ProjectPhase::Drafting, vec![]);
    p.gate_cleared = true; // item 8: the JOIN applies the stashed breakdown once the gate cleared
    let fx = step(
        &mut p,
        invoke_result(InvokePurpose::BreakdownReask, Ok(BREAKDOWN_JSON_OK.to_string())),
        1000,
        4,
    );
    assert!(invoke_effects(&fx).is_empty());
    assert_eq!(p.phase, ProjectPhase::Ready);
    assert_eq!(p.tasks.len(), 1);
}

// ---------------------------------------------------------------------------
// PRD -> research -> TRD -> breakdown pipeline (6.2b)
// ---------------------------------------------------------------------------

#[test]
fn prd_capture_starts_research_in_parallel_gate_off() {
    // ADAPTED (design-speedup item 2): with the gate OFF and research "always", a ```prd fence lands
    // the PRD and spawns research IMMEDIATELY (parallel), and the TRD+CRD authoring join then WAITS
    // for research to settle — no TRD+CRD/breakdown invoke yet.
    let mut p = project(ProjectPhase::Drafting, vec![]);
    p.config.assumption_check = false;
    p.config.research_mode = "always".to_string();
    let reply = "Agreed.\n```prd\n# App\nBuild a CLI.\n```\nShall we?";
    let fx = step(&mut p, invoke_result(InvokePurpose::Persona, Ok(reply.to_string())), 1000, 4);

    assert_eq!(p.prd_markdown, "# App\nBuild a CLI.");
    assert!(fx.iter().any(|e| matches!(e, Effect::SpawnResearch { .. })), "research spawn emitted");
    assert!(invoke_effects(&fx).is_empty(), "no TRD+CRD/breakdown invoke until research settles");
    assert!(p.gate_cleared, "gate off -> the PRD gate immediately clears");
    match &p.research {
        Some(b) => {
            assert_eq!(b.kind, AgentKind::Researcher);
            assert_eq!(b.ext_agent_id, 0, "provisional until the driver reports the real id");
        }
        None => panic!("a research binding must be recorded"),
    }
}

#[test]
fn research_done_status_fetches_the_findings() {
    let mut p = project(ProjectPhase::Drafting, vec![]);
    p.research = Some(researcher_binding(55, 1000));
    let fx = step(&mut p, Input::Host(HostEvent::AgentsDone { agent_id: 55, status: "done".into(), error: None }), 2000, 4);
    assert_eq!(fx, vec![Effect::FetchResult { ext_agent_id: 55 }]);
}

#[test]
fn research_result_settles_the_join_and_authors_trdcrd() {
    // ADAPTED (item 2/3): research completing settles the research side of the join; with the PRD
    // gate already cleared, the COMBINED TRD+CRD authoring invoke fires (not the old TRD-then-CRD).
    let mut p = project(ProjectPhase::Drafting, vec![]);
    p.prd_markdown = "# App\nBuild a Rust CLI.".into();
    p.gate_cleared = true;
    p.research = Some(researcher_binding(55, 1000));
    let report = "preamble\nOFFICE-RESEARCH\nfindings: - use clap v4\n";
    let fx = step(&mut p, Input::Host(HostEvent::Result { agent_id: 55, text: report.into() }), 2000, 4);

    assert!(p.research_notes.contains("clap v4"));
    assert!(p.research.is_none(), "binding cleared once the findings land");
    assert_eq!(sole_invoke_purpose(&fx), InvokePurpose::TrdCrd);
    assert!(p.outbox.iter().any(|n| n.text.contains("research done")));
}

#[test]
fn research_result_before_the_gate_clears_waits_for_the_join() {
    // NEW (item 2): the JOIN — research finishing BEFORE the PRD gate clears stashes the notes and
    // fires NOTHING; the gate clearing later is what authors the docs.
    let mut p = project(ProjectPhase::Drafting, vec![]);
    p.prd_markdown = "# App".into();
    p.gate_cleared = false;
    // Mirrors the real emission: `gate_doc` sets this the instant the PRD-capture step fires the
    // AssumeCheckPrd invoke (kernel.rs), so a genuinely in-process race has it `true` here. Without
    // it, `self_heal_stale_prd_gate` would (correctly, per its OWN contract) treat this as a
    // process-boundary reload and heal immediately, which is exactly what this test must NOT see.
    p.gate_invoke_live_hint = true;
    p.research = Some(researcher_binding(55, 1000));
    let fx = step(&mut p, Input::Host(HostEvent::Result { agent_id: 55, text: "OFFICE-RESEARCH\nfindings: - clap\n".into() }), 2000, 4);
    assert!(p.research.is_none(), "notes stashed, binding cleared");
    assert!(invoke_effects(&fx).is_empty(), "no authoring until the gate clears too");

    let fx2 = step(&mut p, invoke_result(InvokePurpose::AssumeCheckPrd, Ok("ASSUME-CHECK\nverdict: clean\n".into())), 2100, 4);
    assert_eq!(sole_invoke_purpose(&fx2), InvokePurpose::TrdCrd, "gate-clear fires the join");
}

#[test]
fn research_failed_degrades_and_authors_from_the_prd_alone() {
    let mut p = project(ProjectPhase::Drafting, vec![]);
    p.prd_markdown = "# App".into();
    p.gate_cleared = true;
    p.research = Some(researcher_binding(0, 1000)); // provisional; spawn never confirmed
    let fx = step(
        &mut p,
        Input::Host(HostEvent::ResearchFailed { reason: "grant denied".into() }),
        1500,
        4,
    );

    assert!(p.research.is_none());
    assert_eq!(sole_invoke_purpose(&fx), InvokePurpose::TrdCrd, "degrade authors TRD+CRD from the PRD");
    assert!(p
        .outbox
        .iter()
        .any(|n| n.text.contains("research skipped") && n.text.contains("grant denied")));
}

#[test]
fn research_runtime_ceiling_kills_and_degrades() {
    // A hung researcher is force-killed by the reconcile ceiling; with the PRD gate cleared, Drafting
    // degrades to a PRD-only TRD+CRD — the pipeline never wedges on a dead researcher.
    let mut p = project(ProjectPhase::Drafting, vec![]);
    p.prd_markdown = "# App".into();
    p.gate_cleared = true;
    p.research = Some(researcher_binding(700, 0));
    let now = p.config.worker_max_runtime_ms + 5_000;
    let fx = step(&mut p, Input::Host(HostEvent::Reconcile), now, 0);

    assert!(fx.iter().any(|e| matches!(e, Effect::Kill { ext_agent_id: 700 })));
    assert!(p.research.is_none(), "over-age researcher cleared");
    assert_eq!(sole_invoke_purpose(&fx), InvokePurpose::TrdCrd);
    assert!(p.outbox.iter().any(|n| n.text.contains("research skipped")));
}

#[test]
fn stale_pre_migration_gate_cleared_self_heals_on_research_degrade() {
    // Review finding (CRITICAL, migration wedge): a `state.json` persisted before the
    // `gate_cleared` field existed loads with it `false` even though the OLD flow (research only
    // ever ran AFTER the PRD gate passed) had already cleared the gate — clearance was real but
    // never persisted. No `AssumeCheckPrd` invoke will ever be (re-)fired for this PRD in the new
    // build, so the stale researcher binding just settles via the reconcile runtime ceiling
    // (dead-agent path) here. Unhealed, `maybe_author_trdcrd` would no-op forever (silent Drafting
    // wedge) because `gate_cleared` stays `false`. The self-heal must presume the gate cleared and
    // proceed with the TRD+CRD join.
    let mut p = project(ProjectPhase::Drafting, vec![]);
    p.prd_markdown = "# App".into();
    p.gate_cleared = false; // never persisted by the pre-migration build
    p.pending_assumptions.clear(); // nothing waiting on the user — no outcome can ever arrive
    p.research = Some(researcher_binding(700, 0)); // live pre-migration binding, reloaded stale
    let now = p.config.worker_max_runtime_ms + 5_000; // old enough that no gate invoke could still be in flight
    let fx = step(&mut p, Input::Host(HostEvent::Reconcile), now, 0);

    assert!(fx.iter().any(|e| matches!(e, Effect::Kill { ext_agent_id: 700 })));
    assert!(p.research.is_none(), "over-age researcher cleared");
    assert!(p.gate_cleared, "gate presumed cleared by the self-heal");
    assert_eq!(sole_invoke_purpose(&fx), InvokePurpose::TrdCrd, "self-heal unwedges the TRD+CRD join");
    assert!(
        p.trace.iter().any(|e| e.summary.contains("presumed cleared")),
        "self-heal trace recorded"
    );
}

#[test]
fn migrated_project_deserialized_fresh_heals_even_with_a_young_researcher() {
    // Coordinator hardening: age alone misses two real wedges — (1) a fast daemon upgrade kills a
    // research binding that is only minutes old (well under `worker_max_runtime_ms`), and (2) a
    // migrated project whose researcher was ALREADY dead pre-upgrade respawns a FRESH researcher on
    // resume (`resume_should_respawn_research`) that then completes normally at a young age. Both
    // settle with `gate_cleared=false` and a YOUNG research binding — the age belt alone would never
    // fire. The correct signal is the PROCESS BOUNDARY: `gate_invoke_live_hint` is `#[serde(skip)]`,
    // so ANY project deserialized from disk has it `false` regardless of what the in-memory state
    // looked like before the reload — proven here with an ACTUAL serde round-trip (not just a
    // manually-set field), including starting from `true` to show the round-trip is what resets it.
    let mut p = project(ProjectPhase::Drafting, vec![]);
    p.prd_markdown = "# App".into();
    p.gate_cleared = false;
    p.gate_invoke_live_hint = true; // as if a gate invoke WAS in flight when this state was saved
    p.research = Some(researcher_binding(900, 10_000));

    let json = serde_json::to_string(&p).expect("serialize");
    let mut reloaded: Project = serde_json::from_str(&json).expect("deserialize");
    assert!(!reloaded.gate_invoke_live_hint, "the skip field never round-trips through disk");

    // The researcher settles normally, well under `worker_max_runtime_ms` since spawn — the age
    // belt alone would NOT fire here; only the hint (correctly `false` post-reload) heals it.
    let now = 10_500;
    assert!(now - 10_000 < reloaded.config.worker_max_runtime_ms, "settle is young, not ceiling-stale");
    let fx = step(
        &mut reloaded,
        Input::Host(HostEvent::Result { agent_id: 900, text: "OFFICE-RESEARCH\nfindings: - clap\n".into() }),
        now,
        4,
    );

    assert!(reloaded.gate_cleared, "gate presumed cleared by the hint-based self-heal");
    assert_eq!(sole_invoke_purpose(&fx), InvokePurpose::TrdCrd, "self-heal unwedges the TRD+CRD join");
    assert!(
        reloaded.trace.iter().any(|e| e.summary.contains("presumed cleared")),
        "self-heal trace recorded"
    );
}

#[test]
fn in_process_gate_invoke_hint_blocks_heal_even_when_young() {
    // Companion to `research_result_before_the_gate_clears_waits_for_the_join`, added for explicit
    // coverage of the hint as the PRIMARY self-heal signal (not just the age belt): a genuinely
    // in-process PRD gate invoke (`gate_invoke_live_hint = true`, as `gate_doc` sets the instant it
    // fires the AssumeCheckPrd invoke) must NOT be healed away just because research happens to
    // settle first, no matter how young the binding is.
    let mut p = project(ProjectPhase::Drafting, vec![]);
    p.prd_markdown = "# App".into();
    p.gate_cleared = false;
    p.gate_invoke_live_hint = true; // the PRD gate invoke is genuinely in flight THIS process
    p.research = Some(researcher_binding(55, 1000));

    let fx = step(
        &mut p,
        Input::Host(HostEvent::Result { agent_id: 55, text: "OFFICE-RESEARCH\nfindings: - clap\n".into() }),
        2000,
        4,
    );

    assert!(!p.gate_cleared, "hint blocks the self-heal — the invoke may still land");
    assert!(invoke_effects(&fx).is_empty(), "no authoring until the gate itself clears");
}

#[test]
fn trdcrd_result_captures_both_docs_and_gates_them_together() {
    // ADAPTED (item 3): ONE invoke authors BOTH docs; both fences are captured and the SINGLE
    // combined TRD+CRD gate runs over them.
    let mut p = project(ProjectPhase::Drafting, vec![]);
    p.prd_markdown = "# App\nPRD body".into();
    let reply = "Here:\n```trd\n# TRD\nUse axum 0.7.\n```\n```crd\n# CRD\n- README (100 pts)\n```";
    let fx = step(&mut p, invoke_result(InvokePurpose::TrdCrd, Ok(reply.to_string())), 1000, 4);

    assert_eq!(p.trd_markdown, "# TRD\nUse axum 0.7.");
    assert!(p.crd_markdown.contains("README"));
    assert_eq!(sole_invoke_purpose(&fx), InvokePurpose::AssumeCheckTrdCrd, "one combined gate over both docs");
    assert!(p.outbox.iter().any(|n| n.text.contains("TRD + clean-build requirements drafted")));
}

#[test]
fn trdcrd_result_missing_one_fence_nudges_on_the_shared_budget() {
    // NEW (item 3): a long reply missing EITHER fence gets one capture-miss nudge (shared budget)
    // and captures NOTHING until both fences arrive.
    let mut p = project(ProjectPhase::Drafting, vec![]);
    p.prd_markdown = "# App".into();
    let only_trd = format!("```trd\n# TRD\n```\n{}", "narration. ".repeat(60));
    assert!(only_trd.len() > 500);
    let fx = step(&mut p, invoke_result(InvokePurpose::TrdCrd, Ok(only_trd)), 1000, 4);

    assert_eq!(p.capture_nudge_count, 1, "a missing fence nudges (shared budget)");
    assert_eq!(sole_invoke_purpose(&fx), InvokePurpose::TrdCrd, "re-ask for BOTH fences");
    assert!(p.trd_markdown.is_empty(), "nothing captured until both fences arrive");
}

#[test]
fn trdcrd_error_still_proceeds_to_breakdown() {
    // ADAPTED: a TRD+CRD Err proceeds to the breakdown (the gate clears, early breakdown starts),
    // built from the PRD alone — Drafting never wedges.
    let mut p = project(ProjectPhase::Drafting, vec![]);
    p.config.assumption_check = false;
    p.prd_markdown = "# App".into();
    let fx = step(&mut p, invoke_result(InvokePurpose::TrdCrd, Err("model call timed out".into())), 1000, 4);

    assert!(p.trd_markdown.is_empty(), "a failed TRD+CRD leaves the docs empty");
    assert_eq!(sole_invoke_purpose(&fx), InvokePurpose::Breakdown, "TRD+CRD failure -> breakdown from the PRD");
    assert!(p.outbox.iter().any(|n| n.text.contains("TRD+CRD call failed")));
}

#[test]
fn ready_phase_chat_trd_fence_updates_without_breakdown() {
    // A ```trd fence in a normal chat reply (user revised the TRD in conversation) is captured
    // in Ready, but must NOT re-run the breakdown automatically — it points at workflow_breakdown.
    let mut p = project(ProjectPhase::Ready, vec![]);
    let reply = "Revised the TRD:\n```trd\n# TRD v2\nSwitch to Postgres.\n```";
    let fx = step(&mut p, invoke_result(InvokePurpose::Persona, Ok(reply.to_string())), 1000, 4);

    assert_eq!(p.trd_markdown, "# TRD v2\nSwitch to Postgres.");
    assert!(invoke_effects(&fx).is_empty(), "a chat-authored TRD never auto-runs the breakdown");
    assert!(p.outbox.iter().any(|n| n.text.contains("workflow_breakdown")));
}

// ---------------------------------------------------------------------------
// One-shot safeguard gate (design-speedup items 2/4/5 + amendment A)
// ---------------------------------------------------------------------------

#[test]
fn prd_fence_runs_gate_and_research_in_parallel() {
    // ADAPTED (item 2): gate ON + research "always" -> a ```prd fence emits the AssumeCheckPrd gate
    // AND spawns research at the SAME time (research is no longer deferred behind the gate).
    let mut p = project(ProjectPhase::Drafting, vec![]);
    p.config.research_mode = "always".to_string();
    let reply = "```prd\n# App\nBuild a CLI.\n```";
    let fx = step(&mut p, invoke_result(InvokePurpose::Persona, Ok(reply.into())), 1000, 4);

    assert_eq!(p.prd_markdown, "# App\nBuild a CLI.");
    assert!(fx.iter().any(|e| matches!(e, Effect::SpawnResearch { .. })), "research runs in PARALLEL now");
    assert_eq!(sole_invoke_purpose(&fx), InvokePurpose::AssumeCheckPrd);
    assert!(p.research.is_some());
    // REGRESSION: the assume-check must NEVER run in json mode (fail-opens the safeguard).
    match invoke_effects(&fx)[0] {
        Effect::InvokeModel { format, .. } => assert!(format.is_none(), "assume-check must not force json"),
        other => panic!("expected InvokeModel, got {other:?}"),
    }
}

#[test]
fn auto_gate_clean_is_one_invoke() {
    // NEW (amendment A): a CLEAN auto-mode gate is ONE invoke total. research "never" isolates the
    // gate; on clean it clears and authors the docs.
    let mut p = project(ProjectPhase::Drafting, vec![]);
    p.config.research_mode = "never".to_string();
    p.prd_markdown = "# App".into();
    let fx = step(&mut p, invoke_result(InvokePurpose::AssumeCheckPrd, Ok("ASSUME-CHECK\nverdict: clean\n".into())), 1000, 4);
    assert!(
        !invoke_effects(&fx).iter().any(|e| matches!(e, Effect::InvokeModel { purpose: InvokePurpose::AssumeVerify, .. })),
        "clean -> no verify invoke"
    );
    assert_eq!(sole_invoke_purpose(&fx), InvokePurpose::TrdCrd, "clean gate + research skipped -> author");
    assert!(p.gate_cleared);
}

#[test]
fn auto_gate_dirty_resolves_inline_then_verifies_two_invokes_never_three() {
    // NEW (amendment A): a DIRTY auto-mode gate is EXACTLY two invokes — the enumerate (resolves
    // inline + returns the revised doc) and the single verify. Never three.
    let mut p = project(ProjectPhase::Drafting, vec![]);
    p.config.research_mode = "never".to_string();
    p.prd_markdown = "# App v1".into();
    let enumerate = "ASSUME-CHECK\nverdict: assumptions\n- [auto] picked Postgres\n```prd\n# App v2 (Postgres, delegated)\n```";
    let fx = step(&mut p, invoke_result(InvokePurpose::AssumeCheckPrd, Ok(enumerate.into())), 1000, 4);
    assert_eq!(p.prd_markdown, "# App v2 (Postgres, delegated)", "the doc is revised inline (invoke #1)");
    assert_eq!(sole_invoke_purpose(&fx), InvokePurpose::AssumeVerify, "invoke #2 is the verify");
    assert!(p.pending_assumptions.is_empty(), "auto resolution leaves no disk waiting-state");

    let fx2 = step(&mut p, invoke_result(InvokePurpose::AssumeVerify, Ok("ASSUME-CHECK\nverdict: clean\n".into())), 1100, 4);
    assert_eq!(sole_invoke_purpose(&fx2), InvokePurpose::TrdCrd, "verify clean -> author; the WHOLE gate was 2 invokes");
}

#[test]
fn verify_disclose_records_and_never_loops() {
    // NEW (item 5c): a verify that flags NEW items DISCLOSES them and clears the gate — it NEVER
    // triggers another resolve round.
    let mut p = project(ProjectPhase::Drafting, vec![]);
    p.config.research_mode = "never".to_string();
    p.prd_markdown = "# App".into();
    let fx = step(&mut p, invoke_result(InvokePurpose::AssumeVerify, Ok("ASSUME-CHECK\nverdict: assumptions\n- leftover choice\n".into())), 1000, 4);
    assert!(
        !invoke_effects(&fx).iter().any(|e| matches!(e, Effect::InvokeModel { purpose: InvokePurpose::AssumeResolve, .. })),
        "verify never resolves again"
    );
    assert_eq!(sole_invoke_purpose(&fx), InvokePurpose::TrdCrd, "gate clears anyway -> author");
    assert!(p.self_resolved_assumptions.iter().any(|a| a == "leftover choice"), "the new item is disclosed");
    assert!(p.trace.iter().any(|e| e.summary.contains("disclosed")));
}

#[test]
fn ask_mode_critical_items_freeze_before_any_rewrite() {
    // ADAPTED (amendment A): 'ask' mode surfaces CRITICAL items to the user before any rewrite; only
    // the critical item freezes (the non-critical remainder is left for after).
    let mut p = project(ProjectPhase::Drafting, vec![]);
    p.config.assumption_mode = "ask".to_string();
    p.config.research_mode = "never".to_string();
    p.prd_markdown = "# App".into();
    let check = "ASSUME-CHECK\nverdict: assumptions\n- [critical] spends money on SMS\n- [auto] uses Postgres\n";
    let fx = step(&mut p, invoke_result(InvokePurpose::AssumeCheckPrd, Ok(check.into())), 1000, 0);

    assert!(invoke_effects(&fx).is_empty(), "a critical freeze emits no invoke — the user must act");
    assert_eq!(p.pending_assumptions, vec!["spends money on SMS".to_string()], "only the critical item freezes");
    assert!(p.outbox.iter().any(|n| n.text.contains("critical assumption") && n.text.contains("PRD")));
}

#[test]
fn ask_mode_noncritical_resolves_then_verifies() {
    // ADAPTED (amendment A): 'ask' mode with only non-critical items batch-resolves them (a separate
    // resolve invoke), then verifies — enumerate + resolve + verify.
    let mut p = project(ProjectPhase::Drafting, vec![]);
    p.config.assumption_mode = "ask".to_string();
    p.config.research_mode = "never".to_string();
    p.prd_markdown = "# App".into();
    let check = "ASSUME-CHECK\nverdict: assumptions\n- [auto] uses Postgres\n- picked React\n";
    let fx = step(&mut p, invoke_result(InvokePurpose::AssumeCheckPrd, Ok(check.into())), 1000, 0);
    assert_eq!(sole_invoke_purpose(&fx), InvokePurpose::AssumeResolve, "ask resolves the non-critical remainder");
    assert!(p.pending_assumptions.is_empty(), "no freeze on non-critical in ask mode");

    let fx2 = step(&mut p, invoke_result(InvokePurpose::AssumeResolve, Ok("```prd\n# App v2\n```".into())), 1100, 0);
    assert_eq!(p.prd_markdown, "# App v2");
    assert_eq!(sole_invoke_purpose(&fx2), InvokePurpose::AssumeVerify, "resolve -> verify");
}

#[test]
fn assume_check_error_fails_open_and_proceeds() {
    // ADAPTED: a check Err fails open; with research "never" the join authors the docs.
    let mut p = project(ProjectPhase::Drafting, vec![]);
    p.config.research_mode = "never".to_string();
    p.prd_markdown = "# App".into();
    let fx = step(&mut p, invoke_result(InvokePurpose::AssumeCheckPrd, Err("model call timed out".into())), 1000, 4);
    assert_eq!(sole_invoke_purpose(&fx), InvokePurpose::TrdCrd, "a check error FAILS OPEN");
    assert!(p.outbox.iter().any(|n| n.text.contains("assumption check skipped")));
}

#[test]
fn clean_check_clears_prior_pending_assumptions() {
    let mut p = project(ProjectPhase::Drafting, vec![]);
    p.config.research_mode = "never".to_string();
    p.prd_markdown = "# App".into();
    p.pending_assumptions = vec!["stale".to_string()];
    step(&mut p, invoke_result(InvokePurpose::AssumeCheckPrd, Ok("ASSUME-CHECK\nverdict: clean\n".into())), 1000, 0);
    assert!(p.pending_assumptions.is_empty(), "a clean check clears pending_assumptions");
}

#[test]
fn trdcrd_gate_clean_proceeds_to_breakdown() {
    // ADAPTED: the SINGLE combined TRD+CRD gate clearing proceeds to the breakdown.
    let mut p = project(ProjectPhase::Drafting, vec![]);
    p.trd_markdown = "# TRD".into();
    p.crd_markdown = "# CRD".into();
    let fx = step(&mut p, invoke_result(InvokePurpose::AssumeCheckTrdCrd, Ok("ASSUME-CHECK\nverdict: clean\n".into())), 1000, 4);
    assert_eq!(sole_invoke_purpose(&fx), InvokePurpose::Breakdown, "clean TRD+CRD gate -> (early) breakdown");
}

#[test]
fn research_mode_never_skips_research_entirely() {
    // NEW (item 4): "never" skips research; the PRD gate off -> author immediately.
    let mut p = project(ProjectPhase::Drafting, vec![]);
    p.config.assumption_check = false;
    p.config.research_mode = "never".to_string();
    let fx = step(&mut p, invoke_result(InvokePurpose::Persona, Ok("```prd\n# App\n```".into())), 1000, 4);
    assert!(!fx.iter().any(|e| matches!(e, Effect::SpawnResearch { .. })), "never -> no research spawn");
    assert_eq!(sole_invoke_purpose(&fx), InvokePurpose::TrdCrd, "no research -> author immediately");
    assert!(p.trace.iter().any(|e| e.summary.contains("research skipped (config)")));
}

#[test]
fn research_mode_auto_skips_when_stack_is_well_known() {
    // NEW (item 4): "auto" reads the PRD gate's well-known:yes and skips research.
    let mut p = project(ProjectPhase::Drafting, vec![]);
    p.config.research_mode = "auto".to_string();
    p.prd_markdown = "# App".into();
    let fx = step(&mut p, invoke_result(InvokePurpose::AssumeCheckPrd, Ok("ASSUME-CHECK\nverdict: clean\nwell-known: yes\n".into())), 1000, 4);
    assert!(!fx.iter().any(|e| matches!(e, Effect::SpawnResearch { .. })), "well-known -> no research");
    assert_eq!(sole_invoke_purpose(&fx), InvokePurpose::TrdCrd, "skip research -> author");
    assert!(p.trace.iter().any(|e| e.summary.contains("well-known")));
}

#[test]
fn research_mode_auto_runs_when_not_well_known() {
    // NEW (item 4): "auto" + well-known:no runs research; authoring then waits for it.
    let mut p = project(ProjectPhase::Drafting, vec![]);
    p.config.research_mode = "auto".to_string();
    p.prd_markdown = "# App".into();
    let fx = step(&mut p, invoke_result(InvokePurpose::AssumeCheckPrd, Ok("ASSUME-CHECK\nverdict: clean\nwell-known: no\n".into())), 1000, 4);
    assert!(fx.iter().any(|e| matches!(e, Effect::SpawnResearch { .. })), "not well-known -> research runs");
    assert!(invoke_effects(&fx).is_empty(), "authoring waits for research");
}

// ---------------------------------------------------------------------------
// Assumption approval + capture-miss nudge (autonomy loop fix)
// ---------------------------------------------------------------------------

#[test]
fn approval_intent_matches_clear_approvals_and_rejects_negations() {
    // Positives: every deterministic phrase plus natural wrappings; "approve another approach"
    // proves the whole-word negation veto never trips on "another" -> "not".
    for msg in [
        "approve", "Approved", "please approve this", "approve it",
        "you decide", "go ahead", "let's proceed", "LGTM!", "ok go",
        "approve another approach",
    ] {
        assert!(is_approval_intent(msg), "should read as approval: {msg:?}");
    }
    // Negatives: no approval word, OR an approval word paired with a negation — a SAFEGUARD only
    // opens on a CLEAR approval (blanket autonomy is `assumption_mode = "auto"`, not a fuzzy match).
    for msg in [
        "I don't approve of waiting",
        "do not proceed",
        "never approve this",
        "I can't approve yet",
        "reject and rethink",
        "what database should we use?",
    ] {
        assert!(!is_approval_intent(msg), "should NOT read as approval: {msg:?}");
    }
}

#[test]
fn office_message_approval_closes_the_gate_and_clears_pending() {
    let mut p = project(ProjectPhase::Drafting, vec![]);
    p.pending_assumptions = vec!["assumed Postgres".into(), "assumed React".into()];
    let fx = step(&mut p, Input::Command(Command::OfficeMessage { text: "approve".into() }), 1000, 4);

    assert!(p.assumptions_approved, "a clear approval sets the sticky flag");
    assert!(p.pending_assumptions.is_empty(), "pending assumptions are cleared on approval");
    assert!(p.outbox.iter().any(|n| n.text.contains("gate closed")), "a trace notice is queued");
    // The message still drives the persona — it re-emits the doc, which now passes the gate.
    assert!(fx
        .iter()
        .any(|e| matches!(e, Effect::InvokeModel { purpose: InvokePurpose::Persona, .. })));
}

#[test]
fn office_message_non_approval_leaves_the_gate_stopped() {
    let mut p = project(ProjectPhase::Drafting, vec![]);
    p.pending_assumptions = vec!["assumed Postgres".into()];
    step(
        &mut p,
        Input::Command(Command::OfficeMessage { text: "I don't approve of waiting".into() }),
        1000,
        4,
    );
    assert!(!p.assumptions_approved, "an ambiguous/negated message does NOT close the gate");
    assert_eq!(p.pending_assumptions.len(), 1, "pending assumptions remain");
}

#[test]
fn approved_project_skips_the_gate_on_next_capture() {
    // assumption_check ON (default) but the project was already approved: a captured PRD proceeds
    // STRAIGHT to research (no AssumeCheck invoke), the same fail-open shape as the config toggle.
    let mut p = project(ProjectPhase::Drafting, vec![]);
    p.assumptions_approved = true;
    let reply = "```prd\n# App\nBuild a CLI.\n```";
    let fx = step(&mut p, invoke_result(InvokePurpose::Persona, Ok(reply.into())), 1000, 4);

    assert!(fx.iter().any(|e| matches!(e, Effect::SpawnResearch { .. })), "approved -> gate skipped -> research");
    assert!(
        !fx.iter().any(|e| matches!(e, Effect::InvokeModel { purpose: InvokePurpose::AssumeCheckPrd, .. })),
        "no assume-check invoke once approved"
    );
}

#[test]
fn approved_belt_never_stops_even_on_an_in_flight_assumptions_verdict() {
    // A check in flight when the user approved comes back "assumptions" — the belt keeps it from
    // stopping the already-approved pipeline (race protection).
    let mut p = project(ProjectPhase::Drafting, vec![]);
    p.assumptions_approved = true;
    p.prd_markdown = "# App".into();
    let check = "ASSUME-CHECK\nverdict: assumptions\n- assumed Postgres\n";
    let fx = step(&mut p, invoke_result(InvokePurpose::AssumeCheckPrd, Ok(check.into())), 1000, 4);

    assert!(fx.iter().any(|e| matches!(e, Effect::SpawnResearch { .. })), "the approved belt proceeds");
    assert!(p.pending_assumptions.is_empty());
    assert!(p.outbox.iter().any(|n| n.text.contains("no-assume (approved)")));
}

#[test]
fn self_resolved_assumptions_are_capped_at_100() {
    let mut p = project(ProjectPhase::Drafting, vec![]);
    // The approval belt (an in-flight verdict landing after the user approved) is what records
    // flagged items onto the audit trail, so drive the cap through `assumptions_approved`.
    p.assumptions_approved = true;
    p.prd_markdown = "# App".into();
    p.self_resolved_assumptions = (0..99).map(|i| format!("old {i}")).collect();
    // Flagging 5 more pushes to 104 -> capped back to the most recent 100 (oldest excess dropped).
    let check = "ASSUME-CHECK\nverdict: assumptions\n- a\n- b\n- c\n- d\n- e\n";
    step(&mut p, invoke_result(InvokePurpose::AssumeCheckPrd, Ok(check.into())), 1000, 4);

    assert_eq!(p.self_resolved_assumptions.len(), 100, "capped to ~100");
    assert_eq!(p.self_resolved_assumptions.last().unwrap(), "e", "newest kept");
    assert_eq!(p.self_resolved_assumptions.first().unwrap(), "old 4", "oldest excess dropped");
}

#[test]
fn capture_miss_nudges_twice_then_falls_back_to_waiting() {
    // A long Drafting reply with no ```prd fence and no PRD yet is a forgotten-fence PRD: fire a
    // deterministic re-invoke, capped at MAX_CAPTURE_NUDGES, then fall back to surfacing the reply.
    let mut p = project(ProjectPhase::Drafting, vec![]);
    p.prd_markdown = String::new();
    let long_prose = format!("Here is my detailed product thinking. {}", "detail. ".repeat(80));
    assert!(long_prose.len() > 500);

    let fx1 = step(&mut p, invoke_result(InvokePurpose::Persona, Ok(long_prose.clone())), 1000, 4);
    assert_eq!(p.capture_nudge_count, 1);
    let inv1 = invoke_effects(&fx1);
    assert_eq!(inv1.len(), 1, "a nudge re-invoke is fired");
    match inv1[0] {
        Effect::InvokeModel { purpose, system, .. } => {
            assert_eq!(*purpose, InvokePurpose::Persona);
            assert!(system.contains("Emit ONLY the complete document"), "nudge instruction appended to system");
        }
        other => panic!("expected InvokeModel, got {other:?}"),
    }
    assert!(p.outbox.is_empty(), "a nudge does not spam the user's chat");

    let fx2 = step(&mut p, invoke_result(InvokePurpose::Persona, Ok(long_prose.clone())), 1100, 4);
    assert_eq!(p.capture_nudge_count, 2);
    assert_eq!(invoke_effects(&fx2).len(), 1, "second (last) nudge fired");

    let fx3 = step(&mut p, invoke_result(InvokePurpose::Persona, Ok(long_prose.clone())), 1200, 4);
    assert!(invoke_effects(&fx3).is_empty(), "no nudge past the cap");
    assert_eq!(p.capture_nudge_count, 2, "counter stays at the cap (reset only on capture)");
    assert!(p.outbox.iter().any(|n| n.text.starts_with("office[")), "the reply is surfaced as a notice");
}

#[test]
fn capture_miss_does_not_nudge_short_replies() {
    // A short fence-less Drafting reply is a legitimate clarifying question — do NOT nudge; wait.
    let mut p = project(ProjectPhase::Drafting, vec![]);
    let fx = step(&mut p, invoke_result(InvokePurpose::Persona, Ok("Do you want auth?".into())), 1000, 4);
    assert!(invoke_effects(&fx).is_empty(), "short replies are not nudged");
    assert_eq!(p.capture_nudge_count, 0);
    assert!(p.outbox.iter().any(|n| n.text.contains("Do you want auth?")));
}

#[test]
fn capture_nudge_counter_resets_on_successful_prd_capture() {
    let mut p = project(ProjectPhase::Drafting, vec![]);
    p.capture_nudge_count = 2; // as if we had nudged to the cap
    let reply = "```prd\n# App\nBuild it.\n```";
    step(&mut p, invoke_result(InvokePurpose::Persona, Ok(reply.into())), 1000, 4);
    assert_eq!(p.prd_markdown, "# App\nBuild it.");
    assert_eq!(p.capture_nudge_count, 0, "a successful capture resets the nudge cap");
}

// ---------------------------------------------------------------------------
// Safeguard: gate re-run on every reply while pending (feature 1)
// ---------------------------------------------------------------------------

#[test]
fn gated_persona_reply_without_fence_reruns_the_gate() {
    // A STOPPED gate (pending_assumptions set) + a fenceless persona reply re-runs the safeguard
    // on the newest captured doc, so the user's fresh reply (now in the transcript) is re-judged.
    // Live-test 2026-07-15: without this the persona chatted forever and the project wedged.
    let mut p = project(ProjectPhase::Drafting, vec![]);
    p.prd_markdown = "# App\nBuild a CLI.".into();
    p.pending_assumptions = vec!["assumed Postgres".to_string()];
    let reply = "You decide — I'll proceed with my proposed choices."; // no fence
    let fx = step(&mut p, invoke_result(InvokePurpose::Persona, Ok(reply.into())), 1000, 4);

    assert_eq!(sole_invoke_purpose(&fx), InvokePurpose::AssumeCheckPrd, "the PRD gate re-runs");
    // The persona reply still flows to chat.
    assert!(p.office_transcript.iter().any(|m| m.text.contains("You decide")));
    // Pending is unchanged until the re-check verdict lands.
    assert_eq!(p.pending_assumptions, vec!["assumed Postgres".to_string()]);
}

#[test]
fn recheck_clean_clears_pending_and_resumes_deferred_stage() {
    let mut p = project(ProjectPhase::Drafting, vec![]);
    p.prd_markdown = "# App".into();
    p.pending_assumptions = vec!["assumed Postgres".to_string()];
    // A fenceless reply re-emits the PRD gate...
    step(&mut p, invoke_result(InvokePurpose::Persona, Ok("you decide".into())), 1000, 4);
    assert_eq!(p.pending_assumptions.len(), 1, "still pending until the re-check verdict");
    // ...and the re-check comes back clean -> clear + spawn research (the PRD's deferred stage).
    let fx = step(
        &mut p,
        invoke_result(InvokePurpose::AssumeCheckPrd, Ok("ASSUME-CHECK\nverdict: clean\n".into())),
        1100,
        4,
    );
    assert!(p.pending_assumptions.is_empty(), "a clean re-check clears the list");
    assert!(fx.iter().any(|e| matches!(e, Effect::SpawnResearch { .. })), "deferred stage resumes");
    assert!(p
        .outbox
        .iter()
        .any(|n| n.text.contains("assumptions resolved") && n.text.contains("resuming")));
}

#[test]
fn recheck_still_dirty_on_critical_updates_the_pending_list() {
    // ADAPTED (amendment A): 'ask' mode now freezes on CRITICAL items only, so a still-dirty re-check
    // that still flags a [critical] item refreshes the pending list (untagged items would resolve).
    let mut p = project(ProjectPhase::Drafting, vec![]);
    p.config.assumption_mode = "ask".to_string();
    p.config.research_mode = "never".to_string();
    p.prd_markdown = "# App".into();
    p.pending_assumptions = vec!["deploys to prod".to_string(), "spends money".to_string()];
    // Fenceless reply re-emits the gate; the verdict is still dirty but a shorter critical list.
    step(&mut p, invoke_result(InvokePurpose::Persona, Ok("here is my reasoning".into())), 1000, 4);
    let check = "ASSUME-CHECK\nverdict: assumptions\n- [critical] deploys to prod\n";
    let fx = step(&mut p, invoke_result(InvokePurpose::AssumeCheckPrd, Ok(check.into())), 1100, 0);

    assert!(invoke_effects(&fx).is_empty(), "still stopped on the critical item");
    assert_eq!(p.pending_assumptions, vec!["deploys to prod".to_string()], "critical list refreshed (shrank)");
}

// ---------------------------------------------------------------------------
// Safeguard: workflow_approve -> ApproveAssumptions (feature 2)
// ---------------------------------------------------------------------------

#[test]
fn approve_assumptions_clears_resumes_and_records_the_turn() {
    let mut p = project(ProjectPhase::Drafting, vec![]);
    p.config.research_mode = "never".to_string(); // isolate the resume from research
    p.prd_markdown = "# App".into();
    p.pending_assumptions = vec!["assumed Postgres".to_string()];
    let fx = step(&mut p, Input::Command(Command::ApproveAssumptions), 1000, 4);

    assert!(p.pending_assumptions.is_empty(), "human approval clears pending DIRECTLY");
    // ADAPTED: the PRD stage's resume is now the TRD+CRD authoring join (research skipped here).
    assert_eq!(sole_invoke_purpose(&fx), InvokePurpose::TrdCrd, "the deferred stage resumes (no safeguard re-invoke)");
    // The approval is recorded as a User turn so any later gate sees the delegation.
    let last = p.office_transcript.last().expect("a turn was appended");
    assert!(matches!(last.who, ChatAuthor::User), "recorded as a User turn");
    assert!(last.text.contains("Approved"));
    assert!(p
        .outbox
        .iter()
        .any(|n| n.text.contains("approved by user") && n.text.contains("resuming")));
}

#[test]
fn approve_with_nothing_pending_only_notices() {
    let mut p = project(ProjectPhase::Drafting, vec![]);
    p.prd_markdown = "# App".into();
    // pending_assumptions empty by default.
    let fx = step(&mut p, Input::Command(Command::ApproveAssumptions), 1000, 4);

    assert!(!fx.iter().any(|e| matches!(e, Effect::SpawnResearch { .. })), "nothing to resume");
    assert!(invoke_effects(&fx).is_empty());
    assert!(p.outbox.iter().any(|n| n.text.contains("nothing awaiting approval")));
    // The turn is still recorded (the user did act).
    assert!(p.office_transcript.last().map(|m| m.text.contains("Approved")).unwrap_or(false));
}

// ---------------------------------------------------------------------------
// One-shot gate: resolution + verify (design-speedup items 5 + amendment A)
// ---------------------------------------------------------------------------

#[test]
fn auto_mode_dirty_without_inline_revision_still_goes_to_verify() {
    // ADAPTED (amendment A): the auto-mode gate is ONE-SHOT. Even when the enumerate flags [auto]
    // items but returns NO revised fence, the kernel does NOT loop a resolve round — it proceeds to
    // the single verify pass, with no disk waiting-state.
    let mut p = project(ProjectPhase::Drafting, vec![]);
    p.config.research_mode = "never".to_string();
    p.prd_markdown = "# App".into();
    assert_eq!(p.config.assumption_mode, "auto", "default is autonomous");
    let check = "ASSUME-CHECK\nverdict: assumptions\n- [auto] uses Postgres\n- picked React\n";
    let fx = step(&mut p, invoke_result(InvokePurpose::AssumeCheckPrd, Ok(check.into())), 1000, 0);
    assert_eq!(sole_invoke_purpose(&fx), InvokePurpose::AssumeVerify, "auto dirty -> single verify, never a resolve loop");
    assert!(p.pending_assumptions.is_empty(), "auto mode leaves NO disk waiting-state");
}

#[test]
fn ask_resolve_result_revises_the_doc_and_verifies() {
    // ADAPTED: the ask-mode batch resolve invoke returns a revised ```prd -> the PRD is updated in
    // place and the single VERIFY pass runs (NEVER another gate round).
    let mut p = project(ProjectPhase::Drafting, vec![]);
    p.config.assumption_mode = "ask".to_string();
    p.config.research_mode = "never".to_string();
    p.prd_markdown = "# App\nv1".into();
    let revised = "Here is the revised doc:\n```prd\n# App\nv2 — Postgres chosen\n```";
    let fx = step(&mut p, invoke_result(InvokePurpose::AssumeResolve, Ok(revised.into())), 1100, 0);
    assert_eq!(p.prd_markdown, "# App\nv2 — Postgres chosen", "the doc is updated in place");
    assert_eq!(sole_invoke_purpose(&fx), InvokePurpose::AssumeVerify, "resolve -> verify, never re-loop");
}

#[test]
fn ask_full_loop_resolve_verify_then_authors() {
    // Full 'ask' loop: enumerate (non-critical) -> resolve -> verify(clean) -> author. Zero human
    // involvement, and the gate never loops.
    let mut p = project(ProjectPhase::Drafting, vec![]);
    p.config.assumption_mode = "ask".to_string();
    p.config.research_mode = "never".to_string();
    p.prd_markdown = "# App".into();
    step(&mut p, invoke_result(InvokePurpose::AssumeCheckPrd, Ok("ASSUME-CHECK\nverdict: assumptions\n- picked React\n".into())), 1000, 0);
    step(&mut p, invoke_result(InvokePurpose::AssumeResolve, Ok("```prd\n# App\nReact (delegated)\n```".into())), 1100, 0);
    let fx = step(&mut p, invoke_result(InvokePurpose::AssumeVerify, Ok("ASSUME-CHECK\nverdict: clean\n".into())), 1200, 0);
    assert_eq!(sole_invoke_purpose(&fx), InvokePurpose::TrdCrd, "the pipeline authors autonomously");
    assert!(p.pending_assumptions.is_empty());
}

#[test]
fn resolve_error_proceeds_anyway() {
    // A resolution invoke Err never wedges: proceed (research never -> author) + a disclosure notice.
    let mut p = project(ProjectPhase::Drafting, vec![]);
    p.config.research_mode = "never".to_string();
    p.prd_markdown = "# App".into();
    let fx = step(&mut p, invoke_result(InvokePurpose::AssumeResolve, Err("model call timed out".into())), 1100, 0);
    assert_eq!(sole_invoke_purpose(&fx), InvokePurpose::TrdCrd, "an Err resolve fails open");
    assert!(p.outbox.iter().any(|n| n.text.contains("could not finish")));
}

#[test]
fn resolve_missing_fence_proceeds_anyway() {
    let mut p = project(ProjectPhase::Drafting, vec![]);
    p.config.research_mode = "never".to_string();
    p.prd_markdown = "# App".into();
    let fx = step(&mut p, invoke_result(InvokePurpose::AssumeResolve, Ok("I decided everything but forgot the fence.".into())), 1100, 0);
    assert_eq!(sole_invoke_purpose(&fx), InvokePurpose::TrdCrd, "a fenceless resolve fails open");
    assert!(p.outbox.iter().any(|n| n.text.contains("could not finish")));
}

#[test]
fn critical_items_freeze_but_only_the_critical_ones() {
    // A [critical] item stops for the human even in auto mode — pending carries ONLY the critical
    // items (the [auto] ones were resolved inline / dropped this round), and no invoke fires.
    let mut p = project(ProjectPhase::Drafting, vec![]);
    p.config.research_mode = "never".to_string();
    p.prd_markdown = "# App".into();
    let check = "ASSUME-CHECK\nverdict: assumptions\n- [critical] spends money on a paid SMS gateway\n- [auto] uses Postgres\n";
    let fx = step(&mut p, invoke_result(InvokePurpose::AssumeCheckPrd, Ok(check.into())), 1000, 0);
    assert!(invoke_effects(&fx).is_empty(), "a critical freeze emits no invoke");
    assert_eq!(
        p.pending_assumptions,
        vec!["spends money on a paid SMS gateway".to_string()],
        "ONLY the critical item is pending (tag stripped)"
    );
    assert!(p.outbox.iter().any(|n| n.text.contains("critical assumption") && n.text.contains("PRD")));
}

#[test]
fn critical_freeze_is_cleared_by_approve() {
    // The workflow_approve path clears a critical freeze and resumes (research never -> author).
    let mut p = project(ProjectPhase::Drafting, vec![]);
    p.config.research_mode = "never".to_string();
    p.prd_markdown = "# App".into();
    step(&mut p, invoke_result(InvokePurpose::AssumeCheckPrd, Ok("ASSUME-CHECK\nverdict: assumptions\n- [critical] deploys to production\n".into())), 1000, 0);
    assert_eq!(p.pending_assumptions.len(), 1, "frozen on the critical item");
    let fx = step(&mut p, Input::Command(Command::ApproveAssumptions), 1100, 4);
    assert!(p.pending_assumptions.is_empty(), "approval clears the critical freeze");
    assert_eq!(sole_invoke_purpose(&fx), InvokePurpose::TrdCrd, "the pipeline resumes (authors the docs)");
}

#[test]
fn fresh_prd_capture_resets_the_gate() {
    // ADAPTED (design-speedup): a fresh persona ```prd capture reopens the gate for the new doc-set.
    let mut p = project(ProjectPhase::Drafting, vec![]);
    p.gate_cleared = true; // a prior doc-set had cleared
    let reply = "```prd\n# App v2\n```";
    let fx = step(&mut p, invoke_result(InvokePurpose::Persona, Ok(reply.into())), 1000, 4);
    assert!(!p.gate_cleared, "a fresh capture reopens the gate");
    assert_eq!(sole_invoke_purpose(&fx), InvokePurpose::AssumeCheckPrd, "and re-runs the gate on the new doc");
}

#[test]
fn config_set_assumption_mode_roundtrips_and_rejects_unknown_values() {
    let mut p = project(ProjectPhase::Interrupted, vec![]);
    assert_eq!(p.config.assumption_mode, "auto", "default is auto");

    // A valid mode applies.
    step(&mut p, Input::Command(Command::ConfigSet {
        max_workers: None, bounce_budget: None, worker_model: None, reviewer_model: None,
        keep_desks: None, crd_pass_grade: None, assumption_check: None,
        assumption_mode: Some("ask".to_string()),
        research_mode: None,
        drafter_model: None,
    }), 1000, 4);
    assert_eq!(p.config.assumption_mode, "ask", "a valid mode is applied");

    // An unknown value is ignored (treated like an absent field), leaving the current value.
    step(&mut p, Input::Command(Command::ConfigSet {
        max_workers: None, bounce_budget: None, worker_model: None, reviewer_model: None,
        keep_desks: None, crd_pass_grade: None, assumption_check: None,
        assumption_mode: Some("banana".to_string()),
        research_mode: None,
        drafter_model: None,
    }), 1000, 4);
    assert_eq!(p.config.assumption_mode, "ask", "an unknown mode is ignored, not applied");
}

// ---------------------------------------------------------------------------
// Early breakdown + JOIN (design-speedup item 8)
// ---------------------------------------------------------------------------

#[test]
fn early_breakdown_join_builds_board_on_gate_clear() {
    // NEW (item 8): a clean TRD+CRD gate starts the early breakdown AND clears the gate; the JOIN
    // then builds the board when the breakdown lands — so authorize finds a ready board.
    let mut p = project(ProjectPhase::Drafting, vec![]);
    p.trd_markdown = "# TRD".into();
    p.crd_markdown = "# CRD".into();
    let fx = step(&mut p, invoke_result(InvokePurpose::AssumeCheckTrdCrd, Ok("ASSUME-CHECK\nverdict: clean\n".into())), 1000, 4);
    assert_eq!(sole_invoke_purpose(&fx), InvokePurpose::Breakdown, "the early breakdown starts at gate-clear");
    assert!(p.gate_cleared);
    assert!(matches!(p.phase, ProjectPhase::Drafting), "still Drafting until the breakdown lands");
    assert!(p.trace.iter().any(|e| e.summary.contains("started early")));

    let fx2 = step(&mut p, invoke_result(InvokePurpose::Breakdown, Ok(BREAKDOWN_JSON_OK.to_string())), 2000, 4);
    assert!(invoke_effects(&fx2).is_empty());
    assert_eq!(p.phase, ProjectPhase::Ready, "the board is built and the project is Ready to authorize");
    assert_eq!(p.tasks.len(), 1);
    assert!(p.pending_breakdown.is_none(), "the stash was consumed");
}

#[test]
fn early_breakdown_lands_before_verify_and_join_waits() {
    // NEW (item 8): in the DIRTY gate path the early breakdown runs in parallel with the verify; if
    // it lands FIRST it is stashed and applied only once the verify clears the gate.
    let mut p = project(ProjectPhase::Drafting, vec![]);
    p.config.research_mode = "never".into();
    p.trd_markdown = "# TRD".into();
    p.crd_markdown = "# CRD".into();
    let enumerate = "ASSUME-CHECK\nverdict: assumptions\n- [auto] x\n```trd\n# TRD v2\n```\n```crd\n# CRD v2\n```";
    let fx = step(&mut p, invoke_result(InvokePurpose::AssumeCheckTrdCrd, Ok(enumerate.into())), 1000, 4);
    assert!(invoke_effects(&fx).iter().any(|e| matches!(e, Effect::InvokeModel { purpose: InvokePurpose::AssumeVerify, .. })), "verify emitted");
    assert!(invoke_effects(&fx).iter().any(|e| matches!(e, Effect::InvokeModel { purpose: InvokePurpose::Breakdown, .. })), "early breakdown parallel with verify");
    assert!(!p.gate_cleared, "gate not cleared until the verify returns");

    step(&mut p, invoke_result(InvokePurpose::Breakdown, Ok(BREAKDOWN_JSON_OK.to_string())), 1100, 4);
    assert!(matches!(p.phase, ProjectPhase::Drafting), "stashed, not applied while the verify is in flight");
    assert!(p.pending_breakdown.is_some());

    step(&mut p, invoke_result(InvokePurpose::AssumeVerify, Ok("ASSUME-CHECK\nverdict: clean\n".into())), 1200, 4);
    assert_eq!(p.phase, ProjectPhase::Ready, "the verify clearing the gate fires the JOIN");
    assert_eq!(p.tasks.len(), 1);
}

#[test]
fn breakdown_redone_on_fresh_trdcrd_capture() {
    // NEW (item 8): a fresh TRD+CRD capture (a revised doc-set) DISCARDS a stale stashed breakdown
    // and reopens the gate. Gate ON so the fresh capture leaves the gate un-cleared (in flight).
    let mut p = project(ProjectPhase::Drafting, vec![]);
    p.trd_markdown = "# TRD v1".into();
    p.crd_markdown = "# CRD v1".into();
    p.pending_breakdown = Some("stale".into());
    p.gate_cleared = true;
    let reply = "```trd\n# TRD v2\n```\n```crd\n# CRD v2\n```";
    step(&mut p, invoke_result(InvokePurpose::Persona, Ok(reply.into())), 1000, 4);
    assert!(p.pending_breakdown.is_none(), "the stale breakdown is discarded");
    assert!(!p.gate_cleared, "the gate reopens for the revised doc-set");
    assert!(p.trace.iter().any(|e| e.summary.contains("breakdown redone")));
}

#[test]
fn breakdown_failure_then_manual_rerun_builds_the_board() {
    // NEW (item 8 fallback): if the early breakdown fails, the pipeline waits; a manual
    // workflow_breakdown re-run then stashes + applies (the gate already cleared).
    let mut p = project(ProjectPhase::Drafting, vec![]);
    p.trd_markdown = "# TRD".into();
    p.crd_markdown = "# CRD".into();
    p.gate_cleared = true; // gate already cleared, breakdown owed
    step(&mut p, invoke_result(InvokePurpose::Breakdown, Err("bad request".into())), 1000, 4);
    assert!(matches!(p.phase, ProjectPhase::Drafting), "no board yet");
    assert!(p.pending_breakdown.is_none());

    step(&mut p, Input::Command(Command::RequestBreakdown), 1100, 4);
    step(&mut p, invoke_result(InvokePurpose::Breakdown, Ok(BREAKDOWN_JSON_OK.to_string())), 1200, 4);
    assert_eq!(p.phase, ProjectPhase::Ready, "the manual re-run builds the board");
    assert_eq!(p.tasks.len(), 1);
}

#[test]
fn manual_breakdown_in_ready_applies_immediately() {
    // ADAPTED (item 8): a breakdown result in READY (a manual re-plan) applies immediately (replaces
    // the board), rather than stashing.
    let mut p = project(ProjectPhase::Ready, vec![task("old", TaskState::Todo, 0, &[])]);
    let fx = step(&mut p, invoke_result(InvokePurpose::Breakdown, Ok(BREAKDOWN_JSON_OK.to_string())), 1000, 4);
    let _ = fx;
    assert_eq!(p.phase, ProjectPhase::Ready, "stays Ready");
    assert!(p.pending_breakdown.is_none(), "not stashed in Ready");
    assert!(p.tasks.iter().any(|t| t.id.0.ends_with("/t1")), "the board is replaced by the re-plan");
}

#[test]
fn chat_trdcrd_fence_gate_off_proceeds_to_breakdown() {
    // ADAPTED: a chat-authored ```trd + ```crd in Drafting (gate off) captures both and proceeds to
    // the (early) breakdown.
    let mut p = project(ProjectPhase::Drafting, vec![]);
    p.config.assumption_check = false;
    let reply = "Here:\n```trd\n# TRD\n```\n```crd\n# CRD\n- builds clean (100 pts)\n```";
    let fx = step(&mut p, invoke_result(InvokePurpose::Persona, Ok(reply.into())), 1000, 4);
    assert!(p.trd_markdown.contains("# TRD"));
    assert!(p.crd_markdown.contains("builds clean"));
    assert_eq!(sole_invoke_purpose(&fx), InvokePurpose::Breakdown, "gate off -> straight to the early breakdown");
}

// ---------------------------------------------------------------------------
// Clean-build audit gate at completion (6.2c feature B)
// ---------------------------------------------------------------------------

const AUDIT_FAIL_70: &str = "OFFICE-AUDIT\ngrade: 70\nfailures:\n- module utils.rs is unwired\n- debug prints left in main.rs\n";

#[test]
fn last_task_pass_with_crd_spawns_auditor_instead_of_completing() {
    let mut p = project(
        ProjectPhase::Running,
        vec![task(
            "t1",
            TaskState::Review { binding: Some(reviewer_binding(3, 0)), attempt: 1 },
            0,
            &[],
        )],
    );
    p.crd_markdown = "# CRD\n- README present (100 pts)".into();
    let fx = step(&mut p, Input::Host(HostEvent::Result { agent_id: 3, text: REVIEW_PASS.into() }), 1000, 4);

    assert!(matches!(find_task(&p, "t1").state, TaskState::Done { .. }), "the last task is Done");
    assert!(matches!(p.phase, ProjectPhase::Running), "but the project is NOT — the audit gates completion");
    assert!(fx.iter().any(|e| matches!(e, Effect::SpawnAudit { .. })), "the auditor is spawned");
    match &p.audit {
        Some(b) => {
            assert_eq!(b.kind, AgentKind::Auditor);
            assert_eq!(b.ext_agent_id, 0, "provisional until the driver reports the real id");
        }
        None => panic!("an audit binding must be recorded"),
    }
    assert!(p.outbox.iter().any(|n| n.text.contains("clean-build audit")));
}

#[test]
fn last_task_pass_without_crd_completes_normally() {
    let mut p = project(
        ProjectPhase::Running,
        vec![task(
            "t1",
            TaskState::Review { binding: Some(reviewer_binding(3, 0)), attempt: 1 },
            0,
            &[],
        )],
    );
    // crd_markdown empty -> no audit gate.
    let fx = step(&mut p, Input::Host(HostEvent::Result { agent_id: 3, text: REVIEW_PASS.into() }), 1000, 4);
    assert!(matches!(p.phase, ProjectPhase::Done { .. }), "no CRD -> completes immediately");
    assert!(!fx.iter().any(|e| matches!(e, Effect::SpawnAudit { .. })));
    assert!(p.audit.is_none());
}

#[test]
fn audit_spawned_records_the_real_id() {
    let mut p = project(ProjectPhase::Running, vec![task("t1", TaskState::Done { at_ms: 1 }, 0, &[])]);
    p.audit = Some(auditor_binding(0, 100));
    step(&mut p, Input::Host(HostEvent::AuditSpawned { agent_id: 77, spawned_at_ms: 100 }), 100, 4);
    assert_eq!(p.audit.as_ref().unwrap().ext_agent_id, 77);
}

#[test]
fn audit_done_status_fetches_the_verdict() {
    let mut p = project(ProjectPhase::Running, vec![task("t1", TaskState::Done { at_ms: 1 }, 0, &[])]);
    p.crd_markdown = "# CRD".into();
    p.audit = Some(auditor_binding(50, 100));
    let fx = step(&mut p, Input::Host(HostEvent::AgentsDone { agent_id: 50, status: "done".into(), error: None }), 2000, 4);
    assert_eq!(fx, vec![Effect::FetchResult { ext_agent_id: 50 }]);
}

#[test]
fn audit_pass_completes_project_with_grade() {
    let mut p = project(ProjectPhase::Running, vec![task("t1", TaskState::Done { at_ms: 1 }, 0, &[])]);
    p.crd_markdown = "# CRD".into();
    p.audit = Some(auditor_binding(50, 100));
    // Default crd_pass_grade is 98; grade 99 passes.
    step(&mut p, Input::Host(HostEvent::Result { agent_id: 50, text: "OFFICE-AUDIT\ngrade: 99\n".into() }), 2000, 4);
    assert!(matches!(p.phase, ProjectPhase::Done { .. }));
    assert_eq!(p.last_audit_grade, Some(99));
    assert!(p.audit.is_none(), "binding cleared");
    assert!(p.outbox.iter().any(|n| n.text.contains("audit passed") && n.text.contains("99")));
}

#[test]
fn audit_fail_opens_a_todo_remediation_round() {
    let mut p = project(ProjectPhase::Running, vec![task("t1", TaskState::Done { at_ms: 1 }, 0, &[])]);
    p.crd_markdown = "# CRD".into();
    p.audit = Some(auditor_binding(50, 100));
    // cap 0 so the fresh Todo remediation is observed before dispatch could pick it up.
    step(&mut p, Input::Host(HostEvent::Result { agent_id: 50, text: AUDIT_FAIL_70.into() }), 2000, 0);

    assert!(matches!(p.phase, ProjectPhase::Running), "still Running — remediation opened, not Done");
    assert_eq!(p.audit_rounds, 1);
    assert_eq!(p.last_audit_grade, Some(70));
    assert!(p.audit.is_none());
    let rem = p
        .tasks
        .iter()
        .find(|t| t.id.0.contains("crd-remediation-round-1"))
        .expect("a remediation task was created");
    assert!(matches!(rem.state, TaskState::Todo));
    assert_eq!(rem.priority, 100, "remediation is high priority");
    assert!(rem.blocked_by.is_empty(), "no deps");
    assert!(rem.description.contains("utils.rs is unwired"), "the failures are in the task body");
    assert!(p.outbox.iter().any(|n| n.text.contains("remediation round 1")));
}

#[test]
fn remediation_completion_triggers_a_reaudit() {
    // Round 1 audit fails -> Todo remediation. When it passes review and every task is Done
    // again, a fresh auditor is spawned (round 2 re-audit).
    let mut p = project(ProjectPhase::Running, vec![task("t1", TaskState::Done { at_ms: 1 }, 0, &[])]);
    p.crd_markdown = "# CRD".into();
    p.audit = Some(auditor_binding(50, 100));
    step(&mut p, Input::Host(HostEvent::Result { agent_id: 50, text: AUDIT_FAIL_70.into() }), 2000, 0);
    assert_eq!(p.audit_rounds, 1);

    // Drive the remediation task straight to a review verdict.
    let rem_idx = p
        .tasks
        .iter()
        .position(|t| matches!(t.state, TaskState::Todo))
        .expect("remediation todo");
    p.tasks[rem_idx].state = TaskState::Review { binding: Some(reviewer_binding(9, 0)), attempt: 1 };
    let fx = step(&mut p, Input::Host(HostEvent::Result { agent_id: 9, text: REVIEW_PASS.into() }), 3000, 0);

    assert!(fx.iter().any(|e| matches!(e, Effect::SpawnAudit { .. })), "a re-audit is spawned");
    assert!(p.audit.is_some());
    assert!(matches!(p.phase, ProjectPhase::Running), "not Done — the re-audit gates completion");
}

#[test]
fn audit_fail_after_two_rounds_parks_and_halts() {
    let mut p = project(ProjectPhase::Running, vec![task("t1", TaskState::Done { at_ms: 1 }, 0, &[])]);
    p.crd_markdown = "# CRD".into();
    p.audit_rounds = 2; // two automated remediation rounds already tried
    p.audit = Some(auditor_binding(50, 100));
    let fx = step(&mut p, Input::Host(HostEvent::Result { agent_id: 50, text: "OFFICE-AUDIT\ngrade: 60\nfailures:\n- still broken\n".into() }), 2000, 0);

    let parked = p
        .tasks
        .iter()
        .find(|t| matches!(&t.state, TaskState::Parked { reason: ParkReason::AuditFailed(_), .. }))
        .expect("a parked remediation task");
    assert!(parked.id.0.contains("crd-remediation"));
    assert!(matches!(p.phase, ProjectPhase::Halted { .. }), "the parked task halts the line");
    assert!(fx.iter().any(|e| matches!(e, Effect::QueueChatPrompt { .. })));
    assert!(p.outbox.iter().any(|n| n.text.contains("fix manually and unpark")));
}

#[test]
fn auditor_spawn_failure_degrades_to_done() {
    let mut p = project(ProjectPhase::Running, vec![task("t1", TaskState::Done { at_ms: 1 }, 0, &[])]);
    p.crd_markdown = "# CRD".into();
    p.audit = Some(auditor_binding(0, 100)); // provisional; spawn never confirmed
    step(&mut p, Input::Host(HostEvent::AuditFailed { reason: "grant denied".into() }), 1500, 4);
    assert!(matches!(p.phase, ProjectPhase::Done { .. }), "audit skipped -> project done");
    assert!(p.audit.is_none());
    assert!(p.outbox.iter().any(|n| n.text.contains("audit skipped") && n.text.contains("grant denied")));
}

#[test]
fn dead_auditor_degrades_to_done() {
    let mut p = project(ProjectPhase::Running, vec![task("t1", TaskState::Done { at_ms: 1 }, 0, &[])]);
    p.crd_markdown = "# CRD".into();
    p.audit = Some(auditor_binding(50, 100));
    step(&mut p, Input::Host(HostEvent::AgentsDone { agent_id: 50, status: "killed".into(), error: None }), 2000, 4);
    assert!(matches!(p.phase, ProjectPhase::Done { .. }));
    assert!(p.audit.is_none());
    assert!(p.outbox.iter().any(|n| n.text.contains("audit skipped")));
}

#[test]
fn audit_runtime_ceiling_kills_and_degrades_to_done() {
    let mut p = project(ProjectPhase::Running, vec![task("t1", TaskState::Done { at_ms: 1 }, 0, &[])]);
    p.crd_markdown = "# CRD".into();
    p.audit = Some(auditor_binding(700, 0));
    let now = p.config.worker_max_runtime_ms + 5_000;
    let fx = step(&mut p, Input::Host(HostEvent::Reconcile), now, 0);
    assert!(fx.iter().any(|e| matches!(e, Effect::Kill { ext_agent_id: 700 })));
    assert!(p.audit.is_none(), "over-age auditor cleared");
    assert!(matches!(p.phase, ProjectPhase::Done { .. }));
    assert!(p.outbox.iter().any(|n| n.text.contains("audit skipped")));
}

#[test]
fn audit_inconclusive_grade_completes_project() {
    let mut p = project(ProjectPhase::Running, vec![task("t1", TaskState::Done { at_ms: 1 }, 0, &[])]);
    p.crd_markdown = "# CRD".into();
    p.audit = Some(auditor_binding(50, 100));
    step(&mut p, Input::Host(HostEvent::Result { agent_id: 50, text: "OFFICE-AUDIT\ngrade: pending\n".into() }), 2000, 4);
    assert!(matches!(p.phase, ProjectPhase::Done { .. }));
    assert!(p.last_audit_grade.is_none(), "no grade recorded when the audit is inconclusive");
    assert!(p.outbox.iter().any(|n| n.text.contains("inconclusive")));
}

#[test]
fn same_inputs_same_effects() {
    let build = || {
        project(
            ProjectPhase::Running,
            vec![
                task("a", TaskState::Todo, 5, &[]),
                task("b", TaskState::Todo, 5, &[]),
                task("c", TaskState::Todo, 1, &[]),
            ],
        )
    };
    let mut p1 = build();
    let mut p2 = build();
    let fx1 = step(&mut p1, Input::Host(HostEvent::Tick), 1000, 4);
    let fx2 = step(&mut p2, Input::Host(HostEvent::Tick), 1000, 4);
    assert_eq!(fx1, fx2);
    assert_eq!(p1, p2);
}

// ---------------------------------------------------------------------------
// Tracelog (feature: machine diary) + interrupt-from-drafting
// ---------------------------------------------------------------------------

#[test]
fn trace_ring_caps_at_200_dropping_oldest() {
    let mut p = project(ProjectPhase::Drafting, vec![]);
    // Each office message emits at least two trace events (the received line + the invoke it
    // triggers), so 200 messages drives the ring well past its 200-entry cap.
    for i in 0..200u64 {
        step(
            &mut p,
            Input::Command(Command::OfficeMessage { text: format!("msg {i}") }),
            1000 + i,
            4,
        );
    }
    assert_eq!(p.trace.len(), 200, "the ring is capped at 200 entries");
    assert!(
        !p.trace.iter().any(|e| e.summary == "message received: msg 0"),
        "the oldest entries were dropped"
    );
    assert!(
        p.trace.iter().any(|e| e.summary == "message received: msg 199"),
        "the newest entry is retained (newest-last)"
    );
}

#[test]
fn trace_records_capture_gate_and_research() {
    let mut p = project(ProjectPhase::Drafting, vec![]);
    // Gate ON (default): a ```prd fence traces the doc capture (byte count, never body) and the
    // safeguard gate check.
    let reply = "ok\n```prd\n# App\nBuild it.\n```";
    step(&mut p, invoke_result(InvokePurpose::Persona, Ok(reply.to_string())), 1000, 4);
    assert!(
        p.trace.iter().any(|e| e.kind == "capture" && e.summary.starts_with("PRD captured")),
        "PRD capture is traced: {:?}",
        p.trace
    );
    assert!(
        p.trace.iter().any(|e| e.kind == "gate" && e.summary.contains("checking PRD")),
        "the gate check is traced"
    );

    // A clean verdict proceeds to research — both the clean gate and the research spawn are traced.
    step(
        &mut p,
        invoke_result(InvokePurpose::AssumeCheckPrd, Ok("ASSUME-CHECK\nverdict: clean\n".into())),
        1100,
        4,
    );
    assert!(p.trace.iter().any(|e| e.kind == "gate" && e.summary.contains("PRD clean")));
    assert!(p.trace.iter().any(|e| e.kind == "research" && e.summary.starts_with("spawned")));
}

#[test]
fn trace_records_gate_stop_on_critical_assumptions() {
    // ADAPTED (amendment A): the gate now stops only on [critical] items (both modes). Drive two
    // critical items to exercise (and trace) the freeze-and-stop path with its flagged count.
    let mut p = project(ProjectPhase::Drafting, vec![]);
    p.config.research_mode = "never".to_string();
    p.prd_markdown = "# App".into();
    let check = "ASSUME-CHECK\nverdict: assumptions\n- [critical] deploys to prod\n- [critical] spends money\n";
    step(&mut p, invoke_result(InvokePurpose::AssumeCheckPrd, Ok(check.into())), 1000, 0);
    assert!(
        p.trace
            .iter()
            .any(|e| e.kind == "gate" && e.summary.contains("STOPPED") && e.summary.contains("2 assumption")),
        "the critical gate stop records the flagged count: {:?}",
        p.trace
    );
}

#[test]
fn drafting_interrupt_kills_analyst_and_resume_returns_to_drafting() {
    let mut p = project(ProjectPhase::Drafting, vec![]);
    // A live research analyst is in flight; interrupting Drafting must cut it off.
    p.research = Some(researcher_binding(77, 500));
    let fx = step(&mut p, Input::Command(Command::Interrupt { hard: true }), 1000, 4);

    assert!(matches!(p.phase, ProjectPhase::Interrupted));
    assert_eq!(
        p.interrupted_from,
        Some(ProjectPhase::Drafting),
        "the pre-interrupt phase is remembered for resume"
    );
    assert!(
        fx.iter().any(|e| matches!(e, Effect::Kill { ext_agent_id: 77 })),
        "the research analyst is killed"
    );
    assert!(p.research.is_none(), "the research binding is cleared so a late result no-ops");
    assert!(p
        .trace
        .iter()
        .any(|e| e.kind == "phase" && e.summary.contains("hard interrupt from drafting")));

    // Resume returns to Drafting (not forward to Running) and clears the memo.
    step(&mut p, Input::Command(Command::Resume), 1100, 4);
    assert!(matches!(p.phase, ProjectPhase::Drafting), "a drafting-interrupt resumes back to Drafting");
    assert_eq!(p.interrupted_from, None, "the memo is cleared once resumed");
    assert!(p
        .trace
        .iter()
        .any(|e| e.kind == "phase" && e.summary.contains("resumed to drafting")));
}

#[test]
fn invoke_result_is_ignored_while_interrupted() {
    let mut p = project(ProjectPhase::Interrupted, vec![]);
    p.prd_markdown = String::new();
    p.interrupted_from = Some(ProjectPhase::Drafting);
    // A persona reply that WOULD capture a PRD arrives after the interrupt: it must NOT advance
    // the drafting pipeline (the phase is the guard against stale in-flight invokes).
    let reply = "ok\n```prd\n# App\nBuild it.\n```";
    let fx = step(&mut p, invoke_result(InvokePurpose::Persona, Ok(reply.to_string())), 1000, 4);

    assert!(p.prd_markdown.is_empty(), "a stale persona result does not capture a PRD while interrupted");
    assert!(
        !fx.iter().any(|e| matches!(e, Effect::SpawnResearch { .. })),
        "no pipeline effect from a stale result"
    );
    assert!(
        p.trace
            .iter()
            .any(|e| e.kind == "invoke" && e.summary.contains("ignored") && e.summary.contains("interrupted")),
        "the ignored result is recorded on the diary"
    );
}

// ---------------------------------------------------------------------------
// Resume respawns research (design-speedup item 6)
// ---------------------------------------------------------------------------

#[test]
fn resume_respawns_research_when_it_was_mid_research() {
    // NEW (item 6): a hard interrupt during Drafting killed the researcher; resuming with a captured
    // PRD, no notes, and no TRD respawns it immediately instead of waiting for a user message.
    let mut p = project(ProjectPhase::Interrupted, vec![]);
    p.interrupted_from = Some(ProjectPhase::Drafting);
    p.prd_markdown = "# App".into();
    p.research_notes = String::new();
    p.research = None;
    let fx = step(&mut p, Input::Command(Command::Resume), 1000, 4);
    assert!(matches!(p.phase, ProjectPhase::Drafting));
    assert!(fx.iter().any(|e| matches!(e, Effect::SpawnResearch { .. })), "research is respawned on resume");
    assert!(p.research.is_some());
    assert!(p.trace.iter().any(|e| e.kind == "research" && e.summary.contains("respawned on resume")));
}

#[test]
fn resume_does_not_respawn_when_research_already_done() {
    // NEW (item 6): if research already finished (notes present), resume does NOT respawn it.
    let mut p = project(ProjectPhase::Interrupted, vec![]);
    p.interrupted_from = Some(ProjectPhase::Drafting);
    p.prd_markdown = "# App".into();
    p.research_notes = "already researched".into();
    let fx = step(&mut p, Input::Command(Command::Resume), 1000, 4);
    assert!(!fx.iter().any(|e| matches!(e, Effect::SpawnResearch { .. })), "no respawn once research is done");
}

#[test]
fn resume_does_not_respawn_when_research_mode_never() {
    // NEW (item 6): "never" projects never respawn research on resume.
    let mut p = project(ProjectPhase::Interrupted, vec![]);
    p.interrupted_from = Some(ProjectPhase::Drafting);
    p.prd_markdown = "# App".into();
    p.config.research_mode = "never".into();
    let fx = step(&mut p, Input::Command(Command::Resume), 1000, 4);
    assert!(!fx.iter().any(|e| matches!(e, Effect::SpawnResearch { .. })), "never -> no respawn");
}

// ---------------------------------------------------------------------------
// workflow_skip (design-speedup item 7)
// ---------------------------------------------------------------------------

#[test]
fn workflow_skip_kills_research_and_advances_the_join() {
    // NEW (item 7): skip while research is in flight kills the researcher and advances the TRD+CRD
    // authoring join (the PRD gate already cleared here).
    let mut p = project(ProjectPhase::Drafting, vec![]);
    p.prd_markdown = "# App".into();
    p.gate_cleared = true;
    p.research = Some(researcher_binding(88, 500));
    let fx = step(&mut p, Input::Command(Command::SkipResearch), 1000, 4);
    assert!(fx.iter().any(|e| matches!(e, Effect::Kill { ext_agent_id: 88 })), "the researcher is killed");
    assert!(p.research.is_none(), "research binding cleared (skipped)");
    assert_eq!(sole_invoke_purpose(&fx), InvokePurpose::TrdCrd, "the pipeline advances to TRD+CRD authoring");
    assert!(p.trace.iter().any(|e| e.summary.contains("research skipped by user")));
}

#[test]
fn workflow_skip_on_a_migrated_project_also_heals_the_stale_gate() {
    // Coordinator hardening point 6: a user calling workflow_skip on a migrated project (deserialized
    // -> `gate_invoke_live_hint = false`, `gate_cleared = false` never persisted by the pre-migration
    // build) must ALSO unwedge it, exactly like a settle via research_degrade/on_research_result.
    // `skip_research` funnels through the same `maybe_author_trdcrd` -> `self_heal_stale_prd_gate`
    // join, so this locks that path in too.
    let mut p = project(ProjectPhase::Drafting, vec![]);
    p.prd_markdown = "# App".into();
    p.gate_cleared = false; // never persisted by the pre-migration build
    p.gate_invoke_live_hint = false; // deserialized fresh — no invoke can be in flight
    p.research = Some(researcher_binding(88, 500)); // live pre-migration binding, reloaded stale
    let fx = step(&mut p, Input::Command(Command::SkipResearch), 1000, 4); // young settle — age belt would NOT fire
    assert!(fx.iter().any(|e| matches!(e, Effect::Kill { ext_agent_id: 88 })), "the researcher is killed");
    assert!(p.research.is_none(), "research binding cleared (skipped)");
    assert!(p.gate_cleared, "gate presumed cleared by the hint-based self-heal");
    assert_eq!(sole_invoke_purpose(&fx), InvokePurpose::TrdCrd, "the pipeline advances to TRD+CRD authoring");
    assert!(p.trace.iter().any(|e| e.summary.contains("presumed cleared")));
}

#[test]
fn workflow_skip_with_no_research_returns_a_friendly_notice() {
    // NEW (item 7): skip with nothing in flight is a no-op beyond a notice naming the phase.
    let mut p = project(ProjectPhase::Running, vec![]);
    let fx = step(&mut p, Input::Command(Command::SkipResearch), 1000, 4);
    assert!(!fx.iter().any(|e| matches!(e, Effect::Kill { .. })), "nothing to kill");
    assert!(p.outbox.iter().any(|n| n.text.contains("no research is running to skip") && n.text.contains("running")));
}

// ---------------------------------------------------------------------------
// drafter_model routing (design-speedup item 4)
// ---------------------------------------------------------------------------

#[test]
fn drafter_model_routes_doc_drafting_invokes_but_not_the_gate() {
    // NEW (item 4): drafter_model overrides the model on doc-drafting invokes (persona reply,
    // TRD+CRD) but NOT on the gate/safeguard checks.
    let mut p = project(ProjectPhase::Drafting, vec![]);
    p.config.drafter_model = Some("strong-drafter".to_string());
    // A persona reply invoke carries the drafter model.
    let fx = step(&mut p, Input::Command(Command::OfficeMessage { text: "build me a todo app".into() }), 1000, 4);
    match invoke_effects(&fx)[0] {
        Effect::InvokeModel { purpose, model, .. } => {
            assert_eq!(*purpose, InvokePurpose::Persona);
            assert_eq!(model.as_deref(), Some("strong-drafter"), "persona uses the drafter model");
        }
        other => panic!("expected InvokeModel, got {other:?}"),
    }
    // The PRD gate invoke does NOT carry the drafter model.
    let fx2 = step(&mut p, invoke_result(InvokePurpose::Persona, Ok("```prd\n# App\n```".into())), 1100, 4);
    let gate = invoke_effects(&fx2)
        .into_iter()
        .find(|e| matches!(e, Effect::InvokeModel { purpose: InvokePurpose::AssumeCheckPrd, .. }))
        .expect("a gate invoke");
    match gate {
        Effect::InvokeModel { model, .. } => assert!(model.is_none(), "the gate keeps the safeguard role's model"),
        _ => unreachable!(),
    }

    // The combined TRD+CRD authoring invoke also carries the drafter model.
    let mut p2 = project(ProjectPhase::Drafting, vec![]);
    p2.config.drafter_model = Some("strong-drafter".to_string());
    p2.prd_markdown = "# App".into();
    p2.gate_cleared = true;
    p2.research = Some(researcher_binding(9, 1));
    let fx3 = step(&mut p2, Input::Host(HostEvent::Result { agent_id: 9, text: "OFFICE-RESEARCH\nfindings: - x\n".into() }), 1200, 4);
    match invoke_effects(&fx3)[0] {
        Effect::InvokeModel { purpose, model, .. } => {
            assert_eq!(*purpose, InvokePurpose::TrdCrd);
            assert_eq!(model.as_deref(), Some("strong-drafter"), "TRD+CRD authoring uses the drafter model");
        }
        other => panic!("expected InvokeModel, got {other:?}"),
    }
}

#[test]
fn config_set_research_mode_and_drafter_model_roundtrip() {
    // NEW (item 4): the two new config knobs apply (and research_mode rejects unknown values;
    // drafter_model empty-string clears).
    let mut p = project(ProjectPhase::Interrupted, vec![]);
    assert_eq!(p.config.research_mode, "auto", "default research_mode is auto");
    let cfg = |research_mode: Option<&str>, drafter: Option<&str>| Command::ConfigSet {
        max_workers: None, bounce_budget: None, worker_model: None, reviewer_model: None,
        keep_desks: None, crd_pass_grade: None, assumption_check: None, assumption_mode: None,
        research_mode: research_mode.map(str::to_string),
        drafter_model: drafter.map(str::to_string),
    };
    step(&mut p, Input::Command(cfg(Some("always"), Some("m1"))), 1000, 4);
    assert_eq!(p.config.research_mode, "always");
    assert_eq!(p.config.drafter_model.as_deref(), Some("m1"));
    // Unknown research_mode ignored; empty drafter clears.
    step(&mut p, Input::Command(cfg(Some("banana"), Some(""))), 1000, 4);
    assert_eq!(p.config.research_mode, "always", "unknown research_mode ignored");
    assert_eq!(p.config.drafter_model, None, "empty string clears the drafter override");
}
