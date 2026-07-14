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
