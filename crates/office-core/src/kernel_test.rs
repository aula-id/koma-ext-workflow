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
        seq: 0,
    }
}

fn worker_binding(id: u64, at: u64) -> AgentBinding {
    AgentBinding {
        ext_agent_id: id,
        session: "sess-1".to_string(),
        spawned_at_ms: at,
        kind: AgentKind::Worker,
    }
}

fn reviewer_binding(id: u64, at: u64) -> AgentBinding {
    AgentBinding {
        ext_agent_id: id,
        session: "sess-1".to_string(),
        spawned_at_ms: at,
        kind: AgentKind::Reviewer,
    }
}

fn researcher_binding(id: u64, at: u64) -> AgentBinding {
    AgentBinding {
        ext_agent_id: id,
        session: "sess-1".to_string(),
        spawned_at_ms: at,
        kind: AgentKind::Researcher,
    }
}

fn auditor_binding(id: u64, at: u64) -> AgentBinding {
    AgentBinding {
        ext_agent_id: id,
        session: "sess-1".to_string(),
        spawned_at_ms: at,
        kind: AgentKind::Auditor,
    }
}

fn count_spawns(fx: &[Effect]) -> usize {
    fx.iter().filter(|e| matches!(e, Effect::Spawn { .. })).count()
}

fn spawn_agents<'a>(fx: &'a [Effect]) -> Vec<&'a str> {
    fx.iter()
        .filter_map(|e| match e {
            Effect::Spawn { agent, .. } => Some(*agent),
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
    assert!(matches!(&fx[1], Effect::Spawn { agent: "office-worker", .. }));
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
    assert_eq!(spawn_agents(&fx), vec!["office-worker"]);
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
fn prd_capture_spawns_research_and_defers_breakdown() {
    // A ```prd fence in a Drafting reply lands the PRD and kicks off web-research — NOT the
    // breakdown. No models.invoke fires yet (research runs first). assumption_check is disabled
    // here so this test isolates the pipeline mechanics from the 6.2c safeguard gate (which,
    // when on, would sit between the PRD capture and the research spawn — see
    // `prd_capture_runs_assume_check_then_research` for the gated flow).
    let mut p = project(ProjectPhase::Drafting, vec![]);
    p.config.assumption_check = false;
    let reply = "Agreed.\n```prd\n# App\nBuild a CLI.\n```\nShall we?";
    let fx = step(&mut p, invoke_result(InvokePurpose::Persona, Ok(reply.to_string())), 1000, 4);

    assert_eq!(p.prd_markdown, "# App\nBuild a CLI.");
    assert!(fx.iter().any(|e| matches!(e, Effect::SpawnResearch { .. })), "research spawn emitted");
    assert!(invoke_effects(&fx).is_empty(), "no breakdown/TRD invoke until research finishes");
    // A provisional (id 0) project-level research binding is recorded, two-phase.
    match &p.research {
        Some(b) => {
            assert_eq!(b.kind, AgentKind::Researcher);
            assert_eq!(b.ext_agent_id, 0, "provisional until the driver reports the real id");
        }
        None => panic!("a research binding must be recorded"),
    }
    assert!(p.outbox.iter().any(|n| n.text.contains("research")), "notice mentions researching");
}

#[test]
fn research_done_status_fetches_the_findings() {
    let mut p = project(ProjectPhase::Drafting, vec![]);
    p.research = Some(researcher_binding(55, 1000));
    let fx = step(&mut p, Input::Host(HostEvent::AgentsDone { agent_id: 55, status: "done".into() }), 2000, 4);
    assert_eq!(fx, vec![Effect::FetchResult { ext_agent_id: 55 }]);
}

#[test]
fn research_result_stores_capped_notes_and_starts_trd() {
    let mut p = project(ProjectPhase::Drafting, vec![]);
    p.prd_markdown = "# App\nBuild a Rust CLI.".into();
    p.research = Some(researcher_binding(55, 1000));
    let report = "preamble\nOFFICE-RESEARCH\nfindings: - use clap v4\n- ratatui for the TUI\n";
    let fx = step(&mut p, Input::Host(HostEvent::Result { agent_id: 55, text: report.into() }), 2000, 4);

    assert!(p.research_notes.contains("clap v4"));
    assert!(p.research.is_none(), "binding cleared once the findings land");
    assert_eq!(sole_invoke_purpose(&fx), InvokePurpose::Trd);
    assert!(p.outbox.iter().any(|n| n.text.contains("research done")));
}

#[test]
fn research_failed_degrades_to_a_prd_only_trd() {
    let mut p = project(ProjectPhase::Drafting, vec![]);
    p.prd_markdown = "# App".into();
    p.research = Some(researcher_binding(0, 1000)); // provisional; spawn never confirmed
    let fx = step(
        &mut p,
        Input::Host(HostEvent::ResearchFailed { reason: "grant denied".into() }),
        1500,
        4,
    );

    assert!(p.research.is_none());
    assert_eq!(sole_invoke_purpose(&fx), InvokePurpose::Trd, "degrade goes straight to the TRD invoke");
    assert!(p
        .outbox
        .iter()
        .any(|n| n.text.contains("research skipped") && n.text.contains("grant denied")));
}

#[test]
fn research_runtime_ceiling_kills_and_degrades_to_trd() {
    // A hung researcher is force-killed by the reconcile ceiling and Drafting degrades to a
    // PRD-only TRD — the pipeline never wedges on a dead researcher.
    let mut p = project(ProjectPhase::Drafting, vec![]);
    p.prd_markdown = "# App".into();
    p.research = Some(researcher_binding(700, 0));
    let now = p.config.worker_max_runtime_ms + 5_000;
    let fx = step(&mut p, Input::Host(HostEvent::Reconcile), now, 0);

    assert!(fx.iter().any(|e| matches!(e, Effect::Kill { ext_agent_id: 700 })));
    assert!(p.research.is_none(), "over-age researcher cleared");
    assert_eq!(sole_invoke_purpose(&fx), InvokePurpose::Trd);
    assert!(p.outbox.iter().any(|n| n.text.contains("research skipped")));
}

#[test]
fn trd_result_lands_then_crd_then_breakdown_carries_the_trd() {
    // Feature A inserts the CRD between the TRD and the breakdown: a captured TRD proceeds to
    // the CRD invoke (not straight to breakdown), and the breakdown still carries the TRD once
    // the CRD lands. assumption_check is off here to isolate the pipeline from the 6.2c gate.
    let mut p = project(ProjectPhase::Drafting, vec![]);
    p.config.assumption_check = false;
    p.prd_markdown = "# App\nPRD body".into();
    let trd_reply = "Here:\n```trd\n# TRD\nUse axum 0.7 and sqlx.\n```";
    let fx = step(&mut p, invoke_result(InvokePurpose::Trd, Ok(trd_reply.to_string())), 1000, 4);

    assert_eq!(p.trd_markdown, "# TRD\nUse axum 0.7 and sqlx.");
    assert_eq!(sole_invoke_purpose(&fx), InvokePurpose::Crd, "a captured TRD proceeds to the CRD invoke");
    assert!(p.outbox.iter().any(|n| n.text.contains("TRD drafted")));

    // The CRD lands -> the breakdown runs, and its prompt still folds in the TRD.
    let crd_reply = "```crd\n# CRD\n- README present (100 pts)\n```";
    let fx2 = step(&mut p, invoke_result(InvokePurpose::Crd, Ok(crd_reply.to_string())), 2000, 4);
    assert!(p.crd_markdown.contains("README present"));
    let invokes = invoke_effects(&fx2);
    assert_eq!(invokes.len(), 1);
    match invokes[0] {
        Effect::InvokeModel { purpose, prompt, .. } => {
            assert_eq!(*purpose, InvokePurpose::Breakdown);
            assert!(prompt.contains("axum 0.7"), "the TRD is folded into the breakdown prompt: {prompt}");
        }
        other => panic!("expected a breakdown InvokeModel, got {other:?}"),
    }
}

#[test]
fn trd_error_still_proceeds_to_crd_from_the_prd() {
    // A TRD failure (Err / no fence) has nothing to safeguard-check, so it proceeds straight to
    // the CRD invoke (the TRD's successor stage), built from the PRD alone — Drafting never
    // wedges. Previously (pre-6.2c) a TRD Err went directly to breakdown; the CRD now sits in
    // between so the completed project can still be clean-build audited.
    let mut p = project(ProjectPhase::Drafting, vec![]);
    p.config.assumption_check = false;
    p.prd_markdown = "# App".into();
    let fx = step(
        &mut p,
        invoke_result(InvokePurpose::Trd, Err("model call timed out".into())),
        1000,
        4,
    );

    assert!(p.trd_markdown.is_empty(), "a failed TRD leaves trd_markdown empty");
    assert_eq!(sole_invoke_purpose(&fx), InvokePurpose::Crd, "TRD failure proceeds to the CRD, from the PRD alone");
    assert!(p.outbox.iter().any(|n| n.text.contains("TRD call failed")));
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
// Safeguard no-assume gate (6.2c feature C)
// ---------------------------------------------------------------------------

#[test]
fn prd_fence_runs_assume_check_then_spawns_research_on_clean() {
    // Gate ON (default): a ```prd fence emits an AssumeCheckPrd invoke, NOT a research spawn —
    // research is deferred behind the check.
    let mut p = project(ProjectPhase::Drafting, vec![]);
    let reply = "```prd\n# App\nBuild a CLI.\n```";
    let fx = step(&mut p, invoke_result(InvokePurpose::Persona, Ok(reply.into())), 1000, 4);

    assert_eq!(p.prd_markdown, "# App\nBuild a CLI.");
    assert!(!fx.iter().any(|e| matches!(e, Effect::SpawnResearch { .. })), "research is gated");
    assert_eq!(sole_invoke_purpose(&fx), InvokePurpose::AssumeCheckPrd);
    assert!(p.research.is_none(), "no research binding until the check clears");

    // A clean check clears pending_assumptions and spawns research (the deferred stage).
    let fx2 = step(
        &mut p,
        invoke_result(InvokePurpose::AssumeCheckPrd, Ok("ASSUME-CHECK\nverdict: clean\n".into())),
        1100,
        4,
    );
    assert!(fx2.iter().any(|e| matches!(e, Effect::SpawnResearch { .. })), "clean check spawns research");
    assert!(p.research.is_some());
    assert!(p.pending_assumptions.is_empty());
}

#[test]
fn assume_check_assumptions_stops_the_pipeline() {
    let mut p = project(ProjectPhase::Drafting, vec![]);
    p.prd_markdown = "# App".into();
    let check = "ASSUME-CHECK\nverdict: assumptions\n- assumed Postgres\n- assumed React\n";
    let fx = step(&mut p, invoke_result(InvokePurpose::AssumeCheckPrd, Ok(check.into())), 1000, 0);

    assert!(!fx.iter().any(|e| matches!(e, Effect::SpawnResearch { .. })), "assumptions STOP the pipeline");
    assert!(invoke_effects(&fx).is_empty(), "no further invoke — the user must act");
    assert_eq!(
        p.pending_assumptions,
        vec!["assumed Postgres".to_string(), "assumed React".to_string()]
    );
    assert!(p
        .outbox
        .iter()
        .any(|n| n.text.contains("unapproved assumption") && n.text.contains("PRD")));
}

#[test]
fn assume_check_error_fails_open_and_proceeds() {
    let mut p = project(ProjectPhase::Drafting, vec![]);
    p.prd_markdown = "# App".into();
    let fx = step(
        &mut p,
        invoke_result(InvokePurpose::AssumeCheckPrd, Err("model call timed out".into())),
        1000,
        4,
    );
    assert!(fx.iter().any(|e| matches!(e, Effect::SpawnResearch { .. })), "a check error FAILS OPEN");
    assert!(p.outbox.iter().any(|n| n.text.contains("assumption check skipped")));
}

#[test]
fn clean_check_clears_prior_pending_assumptions() {
    let mut p = project(ProjectPhase::Drafting, vec![]);
    p.prd_markdown = "# App".into();
    p.pending_assumptions = vec!["stale".to_string()];
    step(
        &mut p,
        invoke_result(InvokePurpose::AssumeCheckPrd, Ok("ASSUME-CHECK\nverdict: clean\n".into())),
        1000,
        0,
    );
    assert!(p.pending_assumptions.is_empty(), "a clean check clears pending_assumptions");
}

#[test]
fn assume_check_trd_clean_proceeds_to_crd_and_crd_clean_to_breakdown() {
    // The gate's deferred stage is a pure function of the doc: TRD -> CRD, CRD -> breakdown.
    let mut p = project(ProjectPhase::Drafting, vec![]);
    p.trd_markdown = "# TRD".into();
    let fx = step(
        &mut p,
        invoke_result(InvokePurpose::AssumeCheckTrd, Ok("ASSUME-CHECK\nverdict: clean\n".into())),
        1000,
        4,
    );
    assert_eq!(sole_invoke_purpose(&fx), InvokePurpose::Crd);

    p.crd_markdown = "# CRD".into();
    let fx2 = step(
        &mut p,
        invoke_result(InvokePurpose::AssumeCheckCrd, Ok("ASSUME-CHECK\nverdict: clean\n".into())),
        1100,
        4,
    );
    assert_eq!(sole_invoke_purpose(&fx2), InvokePurpose::Breakdown);
}

// ---------------------------------------------------------------------------
// CRD invoke (6.2c feature A)
// ---------------------------------------------------------------------------

#[test]
fn crd_result_fence_gate_off_requests_breakdown() {
    let mut p = project(ProjectPhase::Drafting, vec![]);
    p.config.assumption_check = false;
    p.prd_markdown = "# App".into();
    let fx = step(
        &mut p,
        invoke_result(InvokePurpose::Crd, Ok("```crd\n# CRD\n- README (100 pts)\n```".into())),
        1000,
        4,
    );
    assert!(p.crd_markdown.contains("README"));
    assert_eq!(sole_invoke_purpose(&fx), InvokePurpose::Breakdown);
    assert!(p.outbox.iter().any(|n| n.text.contains("clean-build requirements")));
}

#[test]
fn crd_error_fails_open_to_breakdown_without_audit() {
    let mut p = project(ProjectPhase::Drafting, vec![]);
    p.prd_markdown = "# App".into();
    let fx = step(
        &mut p,
        invoke_result(InvokePurpose::Crd, Err("model call timed out".into())),
        1000,
        4,
    );
    assert!(p.crd_markdown.is_empty(), "a failed CRD leaves crd_markdown empty");
    assert_eq!(sole_invoke_purpose(&fx), InvokePurpose::Breakdown, "CRD failure still breaks down");
    assert!(p.outbox.iter().any(|n| n.text.contains("without a clean-build audit")));
}

#[test]
fn crd_no_fence_fails_open_to_breakdown() {
    let mut p = project(ProjectPhase::Drafting, vec![]);
    p.prd_markdown = "# App".into();
    let fx = step(
        &mut p,
        invoke_result(InvokePurpose::Crd, Ok("here is the crd: build clean".into())),
        1000,
        4,
    );
    assert!(p.crd_markdown.is_empty());
    assert_eq!(sole_invoke_purpose(&fx), InvokePurpose::Breakdown);
    assert!(p.outbox.iter().any(|n| n.text.contains("CRD call skipped")));
}

#[test]
fn chat_crd_fence_gate_off_proceeds_to_breakdown() {
    let mut p = project(ProjectPhase::Drafting, vec![]);
    p.config.assumption_check = false;
    let reply = "Here:\n```crd\n# CRD\n- builds clean (100 pts)\n```";
    let fx = step(&mut p, invoke_result(InvokePurpose::Persona, Ok(reply.into())), 1000, 4);
    assert!(p.crd_markdown.contains("builds clean"));
    assert_eq!(sole_invoke_purpose(&fx), InvokePurpose::Breakdown);
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
    let fx = step(&mut p, Input::Host(HostEvent::AgentsDone { agent_id: 50, status: "done".into() }), 2000, 4);
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
    step(&mut p, Input::Host(HostEvent::AgentsDone { agent_id: 50, status: "killed".into() }), 2000, 4);
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
