//! Store tests (BUILD_WAVES.md W5, ARCHITECTURE.md 4.2). tempfile-rooted: round-trip,
//! atomic-write crash safety, torn journal tail, schema refusal + migration engine, and
//! the self-healing cross-file consistency rules (adopt / drop / quarantine).

use crate::store::*;
use office_core::*;
use serde_json::{json, Value};
use std::fs;
use std::path::PathBuf;
use tempfile::TempDir;

// ---------------------------------------------------------------------------
// Builders
// ---------------------------------------------------------------------------

fn one_task(id: &str) -> Task {
    Task {
        id: TaskId(id.to_string()),
        title: format!("task {}", id),
        description: "do the thing".to_string(),
        acceptance: vec!["it works".to_string()],
        blocked_by: Vec::new(),
        priority: 0,
        state: TaskState::Todo,
        bounces: 0,
        comments: Vec::new(),
        desk: None,
        last_report: None,
        last_review: None,
        history: Vec::new(),
    }
}

fn project(slug: &str) -> Project {
    Project {
        id: ProjectId(slug.to_string()),
        name: format!("Project {}", slug),
        phase: ProjectPhase::Running,
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
        tasks: vec![one_task("t1")],
        config: ProjectConfig::default_config(),
        outbox: Vec::new(),
        seq: 0,
    }
}

fn store() -> (TempDir, Store) {
    let dir = TempDir::new().unwrap();
    let store = Store::open(dir.path()).unwrap();
    (dir, store)
}

// ---------------------------------------------------------------------------
// init
// ---------------------------------------------------------------------------

#[test]
fn open_writes_markers_and_empty_registry() {
    let (dir, store) = store();
    assert!(dir.path().join("README.md").exists());
    assert!(dir.path().join("DO-NOT-ENTER.md").exists());
    assert!(dir.path().join("registry.json").exists());
    assert!(store.registry().unwrap().is_empty());
}

// ---------------------------------------------------------------------------
// round-trip
// ---------------------------------------------------------------------------

#[test]
fn save_load_round_trip() {
    let (_dir, store) = store();
    let p = project("alpha");
    store.save_project(&p).unwrap();

    let loaded = store.load_project("alpha").unwrap();
    assert_eq!(loaded, p);

    let reg = store.registry().unwrap();
    assert_eq!(reg.len(), 1);
    assert_eq!(reg[0].project_id, "alpha");
    assert_eq!(reg[0].phase, "running");
    assert_eq!(reg[0].state_dir, "projects/alpha");
}

// ---------------------------------------------------------------------------
// atomic write: a torn/uncommitted tmp never affects the live file
// ---------------------------------------------------------------------------

#[test]
fn atomic_write_leaves_prior_file_on_mid_write() {
    let (_dir, store) = store();
    let p = project("beta");
    store.save_project(&p).unwrap();

    // Simulate a crash mid-write: a leftover tmp with garbage, never renamed.
    let leftover = store.state_dir("beta").join(".state.json.tmp.999999");
    fs::write(&leftover, b"garbage {not json").unwrap();

    // The live state.json is untouched.
    let loaded = store.load_project("beta").unwrap();
    assert_eq!(loaded, p);
}

// ---------------------------------------------------------------------------
// journal: append + torn-tail tolerance
// ---------------------------------------------------------------------------

#[test]
fn journal_append_and_read() {
    let (_dir, store) = store();
    store.append_journal("gamma", &json!({"ev": "spawn", "n": 1})).unwrap();
    store.append_journal("gamma", &json!({"ev": "done", "n": 2})).unwrap();

    let events = store.read_journal("gamma").unwrap();
    assert_eq!(events.len(), 2);
    assert_eq!(events[0]["ev"], json!("spawn"));
    assert_eq!(events[1]["n"], json!(2));
}

#[test]
fn journal_torn_tail_is_skipped() {
    let (_dir, store) = store();
    store.append_journal("delta", &json!({"ev": "a"})).unwrap();
    store.append_journal("delta", &json!({"ev": "b"})).unwrap();

    // Append a torn final line (a partial JSON write with no newline).
    let path = store.journal_path("delta");
    let mut existing = fs::read(&path).unwrap();
    existing.extend_from_slice(b"{\"ev\": \"c\", partial");
    fs::write(&path, &existing).unwrap();

    let events = store.read_journal("delta").unwrap();
    assert_eq!(events.len(), 2, "torn final line must be skipped");
    assert_eq!(events[1]["ev"], json!("b"));
}

#[test]
fn journal_missing_reads_empty() {
    let (_dir, store) = store();
    assert!(store.read_journal("nope").unwrap().is_empty());
}

// ---------------------------------------------------------------------------
// schema: refuse newer major, migration engine runs
// ---------------------------------------------------------------------------

#[test]
fn newer_schema_major_refused() {
    let (_dir, store) = store();
    let p = project("epsilon");
    store.save_project(&p).unwrap();

    // Bump the on-disk schema to a newer major.
    let sp = store.state_path("epsilon");
    let mut v: Value = serde_json::from_slice(&fs::read(&sp).unwrap()).unwrap();
    v["schema"] = json!("workflow/2");
    fs::write(&sp, serde_json::to_vec(&v).unwrap()).unwrap();

    assert!(store.load_project("epsilon").is_err());
}

#[test]
fn migration_chain_runs_in_order() {
    // The production table is empty (v1), so exercise the engine directly with a
    // synthetic chain to prove the ordered migrations actually run and compose.
    let step1: Migration = |mut v: Value| {
        v["one"] = json!(true);
        v
    };
    let step2: Migration = |mut v: Value| {
        v["two"] = json!(true);
        v
    };
    let out = apply_migrations(json!({"base": 1}), 1, 3, &[step1, step2]).unwrap();
    assert_eq!(out["base"], json!(1));
    assert_eq!(out["one"], json!(true));
    assert_eq!(out["two"], json!(true));

    // No-op when already current.
    let same = apply_migrations(json!({"base": 1}), 1, 1, MIGRATIONS).unwrap();
    assert_eq!(same, json!({"base": 1}));
}

#[test]
fn migration_missing_step_errors() {
    // Asking to migrate past the end of the table is an error, not a silent pass.
    assert!(apply_migrations(json!({}), 1, 2, MIGRATIONS).is_err());
}

// ---------------------------------------------------------------------------
// cross-file self-healing (ARCHITECTURE.md 4.2)
// ---------------------------------------------------------------------------

#[test]
fn create_writes_state_before_registry() {
    // Simulate a crash after state.json lands but before the registry row: put_state only.
    let (_dir, store) = store();
    let p = project("zeta");
    store.put_state(&p).unwrap();
    assert!(store.registry().unwrap().is_empty(), "no registry row yet");

    // Load self-heals: the orphan state is adopted and the registry rebuilt.
    let res = store.load_all().unwrap();
    assert_eq!(res.adopted, vec!["zeta".to_string()]);
    assert_eq!(res.projects.len(), 1);
    assert_eq!(store.registry().unwrap().len(), 1);
}

#[test]
fn registry_row_with_missing_state_is_dropped() {
    // A registry row whose state dir never materialized (torn create the other way).
    let (_dir, store) = store();
    let p = project("eta");
    store.upsert_registry_row(&p).unwrap();
    assert_eq!(store.registry().unwrap().len(), 1);

    let res = store.load_all().unwrap();
    assert_eq!(res.dropped, vec!["eta".to_string()]);
    assert!(res.projects.is_empty());
    assert!(store.registry().unwrap().is_empty(), "dropped row removed from healed registry");
}

#[test]
fn corrupt_state_is_quarantined_and_dropped() {
    let (_dir, store) = store();
    let p = project("theta");
    store.save_project(&p).unwrap();

    // Corrupt the state file in place.
    fs::write(store.state_path("theta"), b"{ this is not valid json").unwrap();

    let res = store.load_all().unwrap();
    assert_eq!(res.quarantined, vec!["theta".to_string()]);
    assert!(res.projects.is_empty());
    // The dir was moved into .quarantine and the row removed.
    assert!(store.root_dir().join("projects/.quarantine/theta").exists());
    assert!(!store.state_dir("theta").exists());
    assert!(store.registry().unwrap().is_empty());
}

#[test]
fn newer_schema_state_is_not_quarantined_or_dropped_by_load_all() {
    // A version-downgrade scenario: a v2 build wrote state.json with a newer schema major,
    // then an older (v1) daemon runs load_all once. The project must NOT be treated as
    // corrupt: no quarantine, no dropped registry row, and it must survive to reappear
    // once a compatible build loads it again (ARCHITECTURE.md 4.2: newer majors are
    // "refused", not conflated with "missing or corrupt").
    let (_dir, store) = store();
    let p = project("nu");
    store.save_project(&p).unwrap();

    // Bump the on-disk schema to a newer major (simulating a v2 write).
    let sp = store.state_path("nu");
    let mut v: Value = serde_json::from_slice(&fs::read(&sp).unwrap()).unwrap();
    v["schema"] = json!("workflow/2");
    fs::write(&sp, serde_json::to_vec(&v).unwrap()).unwrap();

    let res = store.load_all().unwrap();

    // Not loaded into memory (this build genuinely can't read a newer schema)...
    assert!(res.projects.is_empty());
    // ...but explicitly refused, not silently dropped or corrupted.
    assert_eq!(res.newer_schema, vec!["nu".to_string()]);
    assert!(res.quarantined.is_empty(), "must not be quarantined: it isn't corrupt");
    assert!(res.dropped.is_empty(), "must not be dropped: version skew is transient");

    // The state dir stays exactly where it was (not moved to .quarantine).
    assert!(store.state_dir("nu").exists());
    assert!(!store.root_dir().join("projects/.quarantine/nu").exists());
    // The registry row survives the healed rewrite, so the project stays on the board
    // (even though this build can't load its content).
    let reg = store.registry().unwrap();
    assert_eq!(reg.len(), 1);
    assert_eq!(reg[0].project_id, "nu");

    // Once schema support catches back up (simulating re-upgrading to v2), the project
    // loads normally again from the very same on-disk files.
    fs::write(&sp, serde_json::to_vec(&v).unwrap()).unwrap();
    v["schema"] = json!("workflow/1");
    fs::write(&sp, serde_json::to_vec(&v).unwrap()).unwrap();
    let res2 = store.load_all().unwrap();
    assert_eq!(res2.projects.len(), 1);
    assert!(res2.newer_schema.is_empty());
}

#[test]
fn archive_removes_row_before_dir_and_crash_readopts() {
    let (_dir, store) = store();
    let p = project("iota");
    store.save_project(&p).unwrap();

    // Full archive: row gone, dir gone.
    store.archive_project("iota").unwrap();
    assert!(store.registry().unwrap().is_empty());
    assert!(!store.state_dir("iota").exists());

    // Now simulate a crash mid-archive on a fresh project: row removed, dir still present.
    let q = project("kappa");
    store.save_project(&q).unwrap();
    store.remove_registry_row("kappa").unwrap();
    assert!(store.registry().unwrap().is_empty());
    assert!(store.state_dir("kappa").exists());

    // load_all re-adopts the orphaned state dir (archive ordering keeps it adoptable).
    let res = store.load_all().unwrap();
    assert_eq!(res.adopted, vec!["kappa".to_string()]);
    assert_eq!(res.projects.len(), 1);
}

#[test]
fn load_all_keeps_already_registered_projects() {
    let (_dir, store) = store();
    store.save_project(&project("p1")).unwrap();
    store.save_project(&project("p2")).unwrap();

    let res = store.load_all().unwrap();
    assert_eq!(res.projects.len(), 2);
    assert!(res.adopted.is_empty());
    assert!(res.dropped.is_empty());
    assert!(res.quarantined.is_empty());
}

// ---------------------------------------------------------------------------
// symmetric flock (ARCHITECTURE.md 4.4): the lease holder's put_state must contend
// on the SAME state.json.lock a non-holder's with_state_lock takes. Otherwise the lock
// is one-sided and a holder rename can land inside a non-holder's locked window,
// silently clobbering a committed update (the two-session lease safety scenario).
// ---------------------------------------------------------------------------

#[test]
fn holder_put_state_serializes_with_non_holder_lock() {
    use std::sync::mpsc;
    use std::thread;
    use std::time::Duration;

    let (_dir, store) = store();
    let mut v1 = project("race");
    v1.seq = 1;
    store.save_project(&v1).unwrap();

    let (entered_tx, entered_rx) = mpsc::channel::<()>();
    let (proceed_tx, proceed_rx) = mpsc::channel::<()>();

    // Non-holder B: takes the flock, reads v1, then (once released) adds a comment and
    // persists v1+comment. It holds the flock for the WHOLE window between the two signals.
    let b_store = store.clone();
    let b = thread::spawn(move || {
        b_store
            .with_state_lock("race", |p| {
                entered_tx.send(()).unwrap(); // flock held, v1 loaded
                proceed_rx.recv().unwrap(); // stay inside the locked window until released
                p.tasks[0].comments.push(Comment {
                    id: CommentId(1),
                    author: CommentAuthor::User,
                    text: "late comment".to_string(),
                    created_ms: 42,
                    receipt: Receipt::Pending,
                });
            })
            .unwrap();
    });

    // Wait until B holds the flock and has read v1.
    entered_rx.recv().unwrap();

    // Holder A: persist v2 (task Done). With a symmetric flock this BLOCKS until B releases,
    // so A writes last from its own truth. Without the flock A writes immediately and B's
    // later rename clobbers v2 — A's committed completion is silently lost.
    let a_store = store.clone();
    let mut v2 = v1.clone();
    v2.seq = 2;
    v2.tasks[0].state = TaskState::Done { at_ms: 99 };
    let a = thread::spawn(move || {
        a_store.put_state(&v2).unwrap();
    });

    // Give A time to reach (and, once fixed, block on) the flock, then release B.
    thread::sleep(Duration::from_millis(200));
    proceed_tx.send(()).unwrap();

    b.join().unwrap();
    a.join().unwrap();

    // A's committed persist must survive: the two writers serialized on the shared flock.
    let final_state = store.load_project("race").unwrap();
    assert_eq!(final_state.seq, 2, "holder's committed persist was clobbered (one-sided flock)");
    assert!(
        matches!(final_state.tasks[0].state, TaskState::Done { .. }),
        "holder's task-done state was lost to a concurrent non-holder comment add",
    );
}

// ---------------------------------------------------------------------------
// prd mirror
// ---------------------------------------------------------------------------

#[test]
fn prd_write_read() {
    let (_dir, store) = store();
    store.save_project(&project("lambda")).unwrap();
    store.write_prd("lambda", "# Title\ncontent").unwrap();
    assert_eq!(store.read_prd("lambda").unwrap().as_deref(), Some("# Title\ncontent"));
    assert_eq!(store.read_prd("absent").unwrap(), None);
}
