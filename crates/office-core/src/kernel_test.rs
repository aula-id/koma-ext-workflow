//! Kernel tests (ARCHITECTURE.md 5.1-5.3 + BUILD_WAVES.md W4 test plan). The kernel
//! is the correctness core, so this is the heaviest test wave: dispatch/capacity,
//! runtime ceiling, receipt discipline, the full task lifecycle, bounce/park/halt,
//! interrupt/resume, and determinism.

use super::kernel::*;
use crate::domain::*;
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
// Determinism
// ---------------------------------------------------------------------------

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
