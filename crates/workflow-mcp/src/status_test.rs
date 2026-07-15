//! Status-digest tests: seed a real store via office-store in a tempdir, then assert the
//! rendered digest. Also pins the read-only contract (an absent root is never created).

use super::*;
use office_core::{
    AgentBinding, AgentKind, OutboundNotice, Project, ProjectConfig, ProjectId, ProjectPhase,
    Task, TaskId, TaskState,
};
use office_store::Store;
use std::path::PathBuf;

fn task(id: &str, state: TaskState, bounces: u32) -> Task {
    Task {
        id: TaskId(id.to_string()),
        title: format!("title for {id}"),
        description: "desc".to_string(),
        acceptance: vec!["works".to_string()],
        blocked_by: vec![],
        priority: 0,
        state,
        bounces,
        comments: vec![],
        desk: None,
        last_report: None,
        last_review: None,
        history: vec![],
    }
}

fn binding() -> AgentBinding {
    AgentBinding {
        ext_agent_id: 7,
        session: "sess".to_string(),
        spawned_at_ms: 1,
        kind: AgentKind::Worker,
        persona: String::new(),
    }
}

fn project(slug: &str, name: &str, phase: ProjectPhase, tasks: Vec<Task>, seq: u64) -> Project {
    Project {
        id: ProjectId(slug.to_string()),
        name: name.to_string(),
        phase,
        prd_markdown: String::new(),
        trd_markdown: String::new(),
        research_notes: String::new(),
        research: None,
        crd_markdown: String::new(),
        audit: None,
        audit_rounds: 0,
        last_audit_grade: None,
        pending_assumptions: vec![],
        assumptions_approved: false,
        self_resolved_assumptions: vec![],
        capture_nudge_count: 0,
        assumption_rounds: 0,
        office_transcript: vec![],
        office_summary: String::new(),
        delivery_path: Some(PathBuf::from("/ws/deliver")),
        bound_session: Some("sess".to_string()),
        workspace: Some(PathBuf::from("/ws")),
        epics: vec![],
        stories: vec![],
        tasks,
        config: ProjectConfig::default_config(),
        outbox: vec![],
        trace: vec![],
        interrupted_from: None,
        seq,
    }
}

/// Seed a store in a tempdir with a rich "shop" project + a halted "blog", return the root.
fn seed() -> (tempfile::TempDir, PathBuf) {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().to_path_buf();
    let store = Store::open(&root).unwrap();

    let mut shop = project(
        "shop",
        "Online Shop",
        ProjectPhase::Running,
        vec![
            task("shop/e/s/t1", TaskState::Done { at_ms: 1 }, 0),
            task("shop/e/s/t2", TaskState::Todo, 2),
            task(
                "shop/e/s/t3",
                TaskState::Parked { reason: office_core::ParkReason::ReviewBounceBudget, attempt: 1 },
                3,
            ),
            task(
                "shop/e/s/t4",
                TaskState::OnProgress { binding: binding(), attempt: 1 },
                0,
            ),
        ],
        2,
    );
    // One unsent + one sent notice -> exactly one pending.
    shop.outbox = vec![
        OutboundNotice { id: 1, text: "done soon".to_string(), sent: false, paused: false },
        OutboundNotice { id: 2, text: "already sent".to_string(), sent: true, paused: false },
    ];
    // PRD + TRD + CRD present, research not yet run -> digest docs line reads yes/yes/no/yes.
    // A recorded audit grade surfaces on its own line.
    shop.prd_markdown = "# Shop\nbuild the store".to_string();
    shop.trd_markdown = "# TRD\nstack choices".to_string();
    shop.crd_markdown = "# CRD\nclean-build checklist".to_string();
    shop.last_audit_grade = Some(88);
    store.save_project(&shop).unwrap();

    let blog = project(
        "blog",
        "Company Blog",
        ProjectPhase::Halted { reason: "reviewer down".to_string() },
        vec![],
        1,
    );
    store.save_project(&blog).unwrap();

    (tmp, root)
}

#[test]
fn all_projects_digest_reports_counts_parked_bounces_and_outbox() {
    let (_tmp, root) = seed();
    let out = status_digest_at(&root, None);

    assert!(out.contains("Workflow: 2 project(s)"), "{out}");
    assert!(out.contains("shop (Online Shop) - running"), "{out}");
    assert!(
        out.contains("columns: backlog 0 todo 1 onprogress 1 review 1 done 1"),
        "{out}"
    );
    assert!(out.contains("parked: shop/e/s/t3 (bounce budget exceeded)"), "{out}");
    assert!(out.contains("bounces: 5"), "{out}");
    assert!(out.contains("outbox: 1 pending"), "{out}");
    assert!(out.contains("docs: prd yes, trd yes, research no, crd yes"), "{out}");
    assert!(out.contains("audit: 88"), "the last clean-build audit grade surfaces: {out}");

    // Halted phase renders its reason; both projects appear, most-recent (higher seq) first.
    assert!(out.contains("blog (Company Blog) - halted: reviewer down"), "{out}");
    let shop_at = out.find("shop (").unwrap();
    let blog_at = out.find("blog (").unwrap();
    assert!(shop_at < blog_at, "higher-seq shop must sort before blog");
}

#[test]
fn single_project_digest_adds_delivery_and_task_listing() {
    let (_tmp, root) = seed();
    let out = status_digest_at(&root, Some("shop"));

    assert!(out.contains("shop (Online Shop) - running"), "{out}");
    assert!(out.contains("delivery: /ws/deliver"), "{out}");
    assert!(out.contains("tasks:"), "{out}");
    assert!(out.contains("shop/e/s/t2 [todo] title for shop/e/s/t2 (bounces 2)"), "{out}");
    assert!(out.contains("shop/e/s/t4 [onprogress]"), "{out}");
    // A single-project view must not list the other project.
    assert!(!out.contains("Company Blog"), "{out}");
}

#[test]
fn unknown_focus_lists_known_projects() {
    let (_tmp, root) = seed();
    let out = status_digest_at(&root, Some("nope"));
    assert!(out.contains("Project 'nope' not found"), "{out}");
    assert!(out.contains("shop"), "{out}");
    assert!(out.contains("blog"), "{out}");
}

#[test]
fn absent_root_is_reported_without_being_created() {
    let tmp = tempfile::tempdir().unwrap();
    let missing = tmp.path().join("nope-does-not-exist");
    let out = status_digest_at(&missing, None);

    assert!(out.contains("does not exist"), "{out}");
    assert!(!missing.exists(), "status must be read-only: it must not create the root");
}

#[test]
fn empty_store_reports_no_projects() {
    let tmp = tempfile::tempdir().unwrap();
    // An initialized-but-empty store (no projects) reads back cleanly.
    Store::open(tmp.path()).unwrap();
    let out = status_digest_at(tmp.path(), None);
    assert_eq!(out, "No Workflow projects yet.");
}

#[test]
fn research_binding_surfaces_activity_line() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().to_path_buf();
    let store = Store::open(&root).unwrap();

    let mut p = project("shop", "Online Shop", ProjectPhase::Drafting, vec![], 1);
    p.research = Some(AgentBinding {
        ext_agent_id: 7,
        session: "sess".to_string(),
        spawned_at_ms: 1,
        kind: AgentKind::Researcher,
        persona: String::new(),
    });
    store.save_project(&p).unwrap();

    let out = status_digest_at(&root, None);
    assert!(out.contains("activity: researching the stack"), "{out}");
}

#[test]
fn audit_binding_surfaces_activity_line() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().to_path_buf();
    let store = Store::open(&root).unwrap();

    let mut p = project("shop", "Online Shop", ProjectPhase::Running, vec![], 1);
    p.audit = Some(AgentBinding {
        ext_agent_id: 7,
        session: "sess".to_string(),
        spawned_at_ms: 1,
        kind: AgentKind::Auditor,
        persona: String::new(),
    });
    store.save_project(&p).unwrap();

    let out = status_digest_at(&root, None);
    assert!(out.contains("activity: auditing the delivery"), "{out}");
}

#[test]
fn no_activity_line_when_neither_research_nor_audit_is_live() {
    let (_tmp, root) = seed();
    let out = status_digest_at(&root, None);
    assert!(!out.contains("activity:"), "{out}");
}

#[test]
fn pending_assumptions_surface_a_waiting_on_user_line() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().to_path_buf();
    let store = Store::open(&root).unwrap();

    let mut p = project("shop", "Online Shop", ProjectPhase::Drafting, vec![], 1);
    p.pending_assumptions = vec![
        "assumed Postgres".to_string(),
        "assumed a React SPA".to_string(),
    ];
    store.save_project(&p).unwrap();

    let out = status_digest_at(&root, None);
    assert!(out.contains("waiting on user: 2 unapproved assumptions"), "{out}");
}
