//! Lease tests (BUILD_WAVES.md W5, ARCHITECTURE.md 4.4): acquire-when-free, foreign block,
//! stale steal, same-session rebind, heartbeat refresh, and the flock'd concurrent comment
//! add that must serialize two threads without a lost update.

use crate::lease::{self, STALE_MS};
use crate::store::Store;
use office_core::*;
use std::path::PathBuf;
use std::sync::Arc;
use tempfile::TempDir;

fn store() -> (TempDir, Store) {
    let dir = TempDir::new().unwrap();
    let store = Store::open(dir.path()).unwrap();
    (dir, store)
}

#[test]
fn acquire_when_free() {
    let (_dir, store) = store();
    let path = store.lease_path("proj");
    let got = lease::acquire(&path, "inst-a", Some("sess-1"), 100, 1_000).unwrap();
    assert!(got.is_some());
    let l = got.unwrap();
    assert_eq!(l.instance, "inst-a");
    assert_eq!(l.session.as_deref(), Some("sess-1"));
    assert_eq!(l.heartbeat_ms, 1_000);
    assert_eq!(lease::read(&path).unwrap().unwrap(), l);
}

#[test]
fn foreign_live_lease_blocks_second_instance() {
    let (_dir, store) = store();
    let path = store.lease_path("proj");
    assert!(lease::acquire(&path, "inst-a", Some("sess-1"), 100, 1_000).unwrap().is_some());

    // Different instance, different session, not stale -> read-only (None).
    let blocked = lease::acquire(&path, "inst-b", Some("sess-2"), 200, 2_000).unwrap();
    assert!(blocked.is_none());
    // Holder unchanged.
    assert_eq!(lease::read(&path).unwrap().unwrap().instance, "inst-a");
}

#[test]
fn stale_lease_is_stolen() {
    let (_dir, store) = store();
    let path = store.lease_path("proj");
    lease::acquire(&path, "inst-a", Some("sess-1"), 100, 1_000).unwrap();

    // Advance past the steal window; a foreign instance may now take it.
    let now = 1_000 + STALE_MS + 1;
    let stolen = lease::acquire(&path, "inst-b", Some("sess-2"), 200, now).unwrap();
    assert!(stolen.is_some());
    assert_eq!(stolen.unwrap().instance, "inst-b");
}

#[test]
fn just_under_stale_still_blocks() {
    let (_dir, store) = store();
    let path = store.lease_path("proj");
    lease::acquire(&path, "inst-a", Some("sess-1"), 100, 1_000).unwrap();
    let now = 1_000 + STALE_MS; // exactly at the window, not strictly older
    assert!(lease::acquire(&path, "inst-b", Some("sess-2"), 200, now).unwrap().is_none());
}

#[test]
fn same_session_rebind_takes_over_live_lease() {
    // koma restart: same bound session, brand-new instance uuid, old heartbeat still fresh.
    let (_dir, store) = store();
    let path = store.lease_path("proj");
    lease::acquire(&path, "inst-old", Some("sess-x"), 1, 100).unwrap();

    let rebound = lease::acquire(&path, "inst-new", Some("sess-x"), 2, 200).unwrap();
    assert!(rebound.is_some(), "same session must be allowed to rebind");
    assert_eq!(rebound.unwrap().instance, "inst-new");
}

#[test]
fn heartbeat_refreshes_timestamp() {
    let (_dir, store) = store();
    let path = store.lease_path("proj");
    let l = lease::acquire(&path, "inst-a", Some("s"), 1, 100).unwrap().unwrap();

    let refreshed = lease::heartbeat(&path, &l, 5_000).unwrap();
    assert_eq!(refreshed.heartbeat_ms, 5_000);
    assert_eq!(lease::read(&path).unwrap().unwrap().heartbeat_ms, 5_000);
}

#[test]
fn heartbeat_fails_after_foreign_takeover() {
    let (_dir, store) = store();
    let path = store.lease_path("proj");
    let l = lease::acquire(&path, "inst-a", Some("sess-1"), 1, 100).unwrap().unwrap();

    // Someone else stole it (via stale window here), heartbeat by the old owner fails.
    lease::acquire(&path, "inst-b", Some("sess-2"), 2, 100 + STALE_MS + 1).unwrap();
    let err = lease::heartbeat(&path, &l, 100 + STALE_MS + 2);
    assert!(err.is_err());
}

#[test]
fn release_only_when_owned() {
    let (_dir, store) = store();
    let path = store.lease_path("proj");
    lease::acquire(&path, "inst-a", Some("s"), 1, 100).unwrap();

    // A non-owner release is a no-op.
    lease::release(&path, "inst-b").unwrap();
    assert!(lease::read(&path).unwrap().is_some());

    // The owner releases and the file is gone.
    lease::release(&path, "inst-a").unwrap();
    assert!(lease::read(&path).unwrap().is_none());
}

// ---------------------------------------------------------------------------
// flock'd concurrent comment add: two threads must serialize, no lost update
// ---------------------------------------------------------------------------

fn seed_project(store: &Store, slug: &str) {
    let p = Project {
        id: ProjectId(slug.to_string()),
        name: "P".to_string(),
        phase: ProjectPhase::Running,
        prd_markdown: String::new(),
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
        tasks: vec![Task {
            id: TaskId("t1".to_string()),
            title: "t".to_string(),
            description: String::new(),
            acceptance: vec!["ok".to_string()],
            blocked_by: Vec::new(),
            priority: 0,
            state: TaskState::Todo,
            bounces: 0,
            comments: Vec::new(),
            desk: None,
            last_report: None,
            last_review: None,
            history: Vec::new(),
            diff_stat: None,
            awaiting_merge: false,
            dispatch_after_ms: 0,
        }],
        sprints: Vec::new(),
        config: ProjectConfig::default_config(),
        outbox: Vec::new(),
        trace: Vec::new(),
        interrupted_from: None,
        gate_cleared: false,
        gate_invoke_live_hint: false,
        track: "project".to_string(),
        triage_pending: false,
        pending_breakdown: None,
        seq: 0,
        worktree_desks: false,
        workflow_home: None,
        hygiene_sum: 0,
        hygiene_count: 0,
    };
    store.save_project(&p).unwrap();
}

#[test]
fn concurrent_comment_adds_serialize_without_lost_update() {
    let (_dir, store) = store();
    let store = Arc::new(store);
    seed_project(&store, "race");

    let threads: Vec<_> = (0..8)
        .map(|i| {
            let s = Arc::clone(&store);
            std::thread::spawn(move || {
                s.with_state_lock("race", |proj| {
                    let next = proj.tasks[0].comments.len() as u64;
                    proj.tasks[0].comments.push(Comment {
                        id: CommentId(next),
                        author: CommentAuthor::User,
                        text: format!("comment from thread {}", i),
                        created_ms: 0,
                        receipt: Receipt::Pending,
                    });
                })
                .unwrap();
            })
        })
        .collect();

    for t in threads {
        t.join().unwrap();
    }

    // Every add landed (no lost update) and the file is a clean parse (no torn write).
    let loaded = store.load_project("race").unwrap();
    assert_eq!(loaded.tasks[0].comments.len(), 8);
}
