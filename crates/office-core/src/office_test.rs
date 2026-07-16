//! Tests for the front-office persona logic (BUILD_WAVES.md W9): prompt folding,
//! breakdown parse+validate, and the delivery-path authorization gate. All pure — no
//! host, no driver.

use super::*;
use crate::domain::{ChatAuthor, ChatMsg, Project, ProjectConfig, ProjectId, ProjectPhase};
use std::path::PathBuf;

fn project(phase: ProjectPhase) -> Project {
    Project {
        id: ProjectId("shop".to_string()),
        name: "Shop Crawler".to_string(),
        phase,
        prd_markdown: "# PRD\nBuild a crawler.\n".to_string(),
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
        delivery_path: None,
        bound_session: Some("sess-x".to_string()),
        workspace: Some(PathBuf::from("/ws")),
        epics: vec![],
        stories: vec![],
        tasks: vec![],
        sprints: Vec::new(),
        config: ProjectConfig::default_config(),
        outbox: vec![],
        trace: vec![],
        interrupted_from: None,
        gate_cleared: false,
        gate_invoke_live_hint: false,
        track: "project".to_string(),
        triage_pending: false,
        sprint_review_invoke_live: false,
        pending_breakdown: None,
        seq: 1,
        worktree_desks: false,
        workflow_home: None,
        hygiene_sum: 0,
        hygiene_count: 0,
    }
}

fn turn(who: ChatAuthor, text: &str) -> ChatMsg {
    ChatMsg { who, text: text.to_string() }
}

// ---------------------------------------------------------------------------
// build_invoke / folding (6.2)
// ---------------------------------------------------------------------------

#[test]
fn build_invoke_renders_summary_transcript_and_new_message() {
    let mut p = project(ProjectPhase::Drafting);
    p.office_summary = "agreed: crawl shops nightly".to_string();
    p.office_transcript = vec![turn(ChatAuthor::User, "hi"), turn(ChatAuthor::Office, "hello")];

    let (system, prompt) = office::build_invoke(&p, "what next?");
    assert!(system.contains("front office"), "system carries the persona head");
    assert!(prompt.contains("agreed: crawl shops nightly"), "summary is included");
    assert!(prompt.contains("User: hi") && prompt.contains("Office: hello"), "transcript is rendered");
    assert!(prompt.contains("User: what next?"), "the new user message is appended");
}

#[test]
fn should_fold_true_over_threshold_and_build_invoke_stays_under_hard_cap() {
    let mut p = project(ProjectPhase::Drafting);
    // Many fat turns push the assembled prompt well past FOLD_THRESHOLD and HARD_PROMPT_CAP.
    for i in 0..200 {
        p.office_transcript.push(turn(ChatAuthor::User, &format!("message {i} {}", "x".repeat(400))));
        p.office_transcript.push(turn(ChatAuthor::Office, &format!("reply {i} {}", "y".repeat(400))));
    }

    assert!(office::should_fold(&p, ""), "an oversized transcript triggers folding");

    let (_system, prompt) = office::build_invoke(&p, "");
    assert!(
        prompt.len() <= office::HARD_PROMPT_CAP,
        "build_invoke hard-truncates to the 32KB cap even on a pathological transcript (was {})",
        prompt.len()
    );
}

#[test]
fn should_fold_false_for_short_conversation() {
    let mut p = project(ProjectPhase::Drafting);
    p.office_transcript = vec![turn(ChatAuthor::User, "hi"), turn(ChatAuthor::Office, "hello")];
    assert!(!office::should_fold(&p, "one more question"));
}

#[test]
fn apply_fold_replaces_summary_and_keeps_newest_turns() {
    let mut p = project(ProjectPhase::Drafting);
    for i in 0..10 {
        p.office_transcript.push(turn(ChatAuthor::User, &format!("u{i}")));
    }
    let newest = p.office_transcript.last().unwrap().text.clone();
    let original_len = p.office_transcript.len();

    office::apply_fold(&mut p, "folded summary of the first half".to_string());

    assert_eq!(p.office_summary, "folded summary of the first half");
    assert_eq!(p.office_transcript.len(), original_len - original_len / 2, "the oldest half is dropped");
    assert_eq!(
        p.office_transcript.last().unwrap().text,
        newest,
        "the newest turn survives the fold verbatim"
    );
}

#[test]
fn build_fold_summarizes_the_oldest_half() {
    let mut p = project(ProjectPhase::Drafting);
    p.office_transcript = vec![
        turn(ChatAuthor::User, "OLDEST"),
        turn(ChatAuthor::Office, "second"),
        turn(ChatAuthor::User, "third"),
        turn(ChatAuthor::Office, "NEWEST"),
    ];
    let (system, prompt) = office::build_fold(&p);
    assert!(system.contains("compress"), "fold system asks for a summary");
    assert!(prompt.contains("OLDEST"), "the oldest turn is in the fold set");
    assert!(!prompt.contains("NEWEST"), "the newest turn is NOT folded (it stays live)");
}

// ---------------------------------------------------------------------------
// parse_breakdown (6.3.2)
// ---------------------------------------------------------------------------

const GOOD: &str = r#"
{
  "epics": [
    { "slug": "ingest", "title": "Ingest", "intent": "pull data", "stories": [
      { "slug": "fetch", "title": "Fetch", "intent": "http", "tasks": [
        { "slug": "client", "title": "HTTP client", "description": "d", "acceptance": ["compiles"], "priority": 5, "blocked_by": [] },
        { "slug": "retry", "title": "Retry logic", "description": "d", "acceptance": ["retries"], "priority": 1, "blocked_by": ["client"] }
      ]}
    ]}
  ]
}
"#;

#[test]
fn parse_breakdown_accepts_valid_json_and_apply_lands_the_board_ready() {
    let b = office::parse_breakdown(GOOD).expect("valid breakdown");
    let mut p = project(ProjectPhase::Drafting);
    office::apply_breakdown(&mut p, b);

    assert_eq!(p.phase, ProjectPhase::Ready, "landing the breakdown moves Drafting -> Ready");
    assert_eq!(p.epics.len(), 1);
    assert_eq!(p.stories.len(), 1);
    assert_eq!(p.tasks.len(), 2);
    // Hierarchical ids are rebuilt with the project prefix.
    assert_eq!(p.tasks[0].id.0, "shop/ingest/fetch/client");
    // blocked_by is resolved from the slug to the full task id.
    assert_eq!(p.tasks[1].blocked_by, vec![office_core_task_id("shop/ingest/fetch/client")]);
    // Every task is groomed to Todo.
    assert!(p.tasks.iter().all(|t| matches!(t.state, crate::domain::TaskState::Todo)));
}

fn office_core_task_id(s: &str) -> crate::domain::TaskId {
    crate::domain::TaskId(s.to_string())
}

#[test]
fn parse_breakdown_tolerates_prose_and_code_fences() {
    let fenced = format!("Sure! Here is the plan:\n```json\n{}\n```\nHope that helps.", GOOD);
    assert!(office::parse_breakdown(&fenced).is_ok(), "surrounding prose/fences are stripped");
}

#[test]
fn parse_breakdown_rejects_cyclic_deps() {
    let cyclic = r#"{"epics":[{"slug":"e","stories":[{"slug":"s","tasks":[
      {"slug":"a","acceptance":["x"],"blocked_by":["b"]},
      {"slug":"b","acceptance":["x"],"blocked_by":["a"]}
    ]}]}]}"#;
    match office::parse_breakdown(cyclic) {
        Err(office::BreakdownError::Cycle(_)) => {}
        other => panic!("expected Cycle, got {other:?}"),
    }
}

#[test]
fn parse_breakdown_rejects_duplicate_slugs() {
    let dup = r#"{"epics":[{"slug":"e","stories":[{"slug":"s","tasks":[
      {"slug":"a","acceptance":["x"]},
      {"slug":"a","acceptance":["y"]}
    ]}]}]}"#;
    match office::parse_breakdown(dup) {
        Err(office::BreakdownError::DuplicateSlug(s)) => assert_eq!(s, "a"),
        other => panic!("expected DuplicateSlug, got {other:?}"),
    }
}

#[test]
fn parse_breakdown_rejects_empty_acceptance() {
    let empty = r#"{"epics":[{"slug":"e","stories":[{"slug":"s","tasks":[
      {"slug":"a","acceptance":[]}
    ]}]}]}"#;
    match office::parse_breakdown(empty) {
        Err(office::BreakdownError::EmptyAcceptance(s)) => assert_eq!(s, "a"),
        other => panic!("expected EmptyAcceptance, got {other:?}"),
    }
}

#[test]
fn parse_breakdown_rejects_unknown_blocked_by_ref() {
    let bad = r#"{"epics":[{"slug":"e","stories":[{"slug":"s","tasks":[
      {"slug":"a","acceptance":["x"],"blocked_by":["ghost"]}
    ]}]}]}"#;
    match office::parse_breakdown(bad) {
        Err(office::BreakdownError::UnknownRef(s)) => assert_eq!(s, "ghost"),
        other => panic!("expected UnknownRef, got {other:?}"),
    }
}

#[test]
fn parse_breakdown_rejects_malformed_json_for_reask() {
    // Malformed -> a Json error the kernel quotes into its single re-ask (6.3.2).
    match office::parse_breakdown("not json at all {{{") {
        Err(office::BreakdownError::Json(_)) => {}
        other => panic!("expected Json parse error, got {other:?}"),
    }
}

#[test]
fn parse_breakdown_rejects_empty_plan() {
    match office::parse_breakdown(r#"{"epics":[]}"#) {
        Err(office::BreakdownError::Empty) => {}
        other => panic!("expected Empty, got {other:?}"),
    }
}

// ---------------------------------------------------------------------------
// authorization (6.3.3)
// ---------------------------------------------------------------------------

#[test]
fn authorize_valid_path_inside_workspace_starts_running() {
    let mut p = project(ProjectPhase::Ready);
    let path = PathBuf::from("/ws/deliver");
    office::authorize(&mut p, path.clone(), false).expect("authorized");
    assert_eq!(p.phase, ProjectPhase::Running);
    assert_eq!(p.delivery_path, Some(path));
}

#[test]
fn authorize_relative_path_is_rejected() {
    let mut p = project(ProjectPhase::Ready);
    let err = office::authorize(&mut p, PathBuf::from("deliver/here"), false).unwrap_err();
    assert_eq!(err, office::AuthError::NotAbsolute);
    assert_eq!(p.phase, ProjectPhase::Ready, "a rejected authorize leaves the project in Ready");
    assert!(p.delivery_path.is_none());
}

#[test]
fn authorize_outside_workspace_blocked_unless_escape_hatch() {
    let mut p = project(ProjectPhase::Ready);
    // Outside the /ws workspace, escape hatch OFF -> blocked.
    let err = office::authorize(&mut p, PathBuf::from("/elsewhere/out"), false).unwrap_err();
    assert_eq!(err, office::AuthError::OutsideWorkspace);
    assert_eq!(p.phase, ProjectPhase::Ready);

    // Same path, escape hatch ON -> allowed.
    office::authorize(&mut p, PathBuf::from("/elsewhere/out"), true).expect("escape hatch authorizes");
    assert_eq!(p.phase, ProjectPhase::Running);
}

#[test]
fn validate_delivery_path_requires_a_known_workspace() {
    let err = office::validate_delivery_path(&PathBuf::from("/ws/x"), None, false).unwrap_err();
    assert_eq!(err, office::AuthError::NoWorkspace);
}

#[test]
fn authorize_from_wrong_phase_is_rejected() {
    let mut p = project(ProjectPhase::Drafting);
    let err = office::authorize(&mut p, PathBuf::from("/ws/deliver"), false).unwrap_err();
    assert_eq!(err, office::AuthError::WrongPhase);
}

// ---- extract_prd: the ```prd fence capture contract (6.2) ----

#[test]
fn extract_prd_captures_a_fenced_block() {
    let reply = "Sounds good.\n```prd\n# Title\nBody line.\n```\nAnything else?";
    assert_eq!(office::extract_prd(reply).as_deref(), Some("# Title\nBody line."));
}

#[test]
fn extract_prd_takes_the_last_fence_and_ignores_prose() {
    let reply = "draft one\n```prd\nold\n```\nrevised:\n```prd\nnew version\n```";
    assert_eq!(office::extract_prd(reply).as_deref(), Some("new version"));
    assert_eq!(office::extract_prd("here is the PRD: build a todo app"), None);
}

#[test]
fn extract_prd_tolerates_an_unterminated_fence_and_rejects_empty() {
    assert_eq!(
        office::extract_prd("```prd\ncontent till end").as_deref(),
        Some("content till end")
    );
    assert_eq!(office::extract_prd("```prd\n\n```"), None);
    assert_eq!(office::extract_prd(""), None);
}

// ---- extract_fenced generalization + TRD / research pipeline (6.2b) ----

#[test]
fn extract_fenced_generalizes_prd_and_trd_capture() {
    assert_eq!(office::extract_fenced("```prd\n# P\nbody\n```", "prd").as_deref(), Some("# P\nbody"));
    assert_eq!(office::extract_fenced("```trd\n# T\nstack\n```", "trd").as_deref(), Some("# T\nstack"));
    // The wrong tag never matches the other's fence.
    assert_eq!(office::extract_fenced("```prd\nx\n```", "trd"), None);
    // Last fence wins, prose ignored — same contract as extract_prd.
    assert_eq!(
        office::extract_fenced("```trd\nold\n```\nrevised:\n```trd\nnew\n```", "trd").as_deref(),
        Some("new")
    );
    // extract_prd is the thin wrapper over extract_fenced(_, "prd").
    assert_eq!(office::extract_prd("```prd\nY\n```").as_deref(), Some("Y"));
}

// ---- extract_fenced hardening: case, trailing text, embedded code blocks ----

#[test]
fn extract_fenced_keeps_embedded_code_blocks() {
    // A PRD that contains an example ```rust fenced block must NOT truncate at the FIRST closing
    // ``` (the embedded block's) — capture greedily to the LAST lone closing fence.
    let reply = "```prd\n# PRD\nExample:\n```rust\nfn main() {}\n```\nDone.\n```";
    assert_eq!(
        office::extract_fenced(reply, "prd").as_deref(),
        Some("# PRD\nExample:\n```rust\nfn main() {}\n```\nDone.")
    );
}

#[test]
fn extract_fenced_tag_match_is_case_insensitive() {
    assert_eq!(office::extract_fenced("```PRD\n# P\n```", "prd").as_deref(), Some("# P"));
    assert_eq!(office::extract_fenced("```Trd\nstack\n```", "trd").as_deref(), Some("stack"));
    // A different tag still never matches, case aside.
    assert_eq!(office::extract_fenced("```PRD\nx\n```", "trd"), None);
    // The tag must be the WHOLE first token — a prefix like ```prdx is not a ```prd fence.
    assert_eq!(office::extract_fenced("```prdx\nx\n```", "prd"), None);
}

#[test]
fn extract_fenced_tolerates_trailing_text_after_the_tag() {
    // A language/annotation after the tag on the fence line is ignored; the body is still captured.
    assert_eq!(
        office::extract_fenced("```prd markdown\n# P\nbody\n```", "prd").as_deref(),
        Some("# P\nbody")
    );
    assert_eq!(
        office::extract_fenced("```prd (final draft)\nX\n```", "prd").as_deref(),
        Some("X")
    );
}

#[test]
fn build_trdcrd_prompt_folds_prd_and_research_and_asks_for_both_fences() {
    // ADAPTED (design-speedup item 3): the old separate build_trd_prompt/build_crd_prompt are now
    // ONE combined authoring invoke that must emit BOTH ```trd and ```crd.
    let mut p = project(ProjectPhase::Drafting);
    p.prd_markdown = "# PRD\nCrawler.".to_string();
    p.research_notes = "use reqwest 0.12".to_string();
    let (system, prompt) = office::build_trdcrd_prompt(&p);
    assert!(system.contains("front office"));
    assert!(prompt.contains("Crawler"), "PRD is included");
    assert!(prompt.contains("reqwest 0.12"), "research notes are included when present");
    assert!(prompt.contains("```trd"), "the ```trd capture contract is stated");
    assert!(prompt.contains("```crd"), "the ```crd capture contract is stated");
    assert!(prompt.contains("SUM TO EXACTLY 100"), "the CRD rubric contract is stated");
    assert!(prompt.len() <= office::HARD_PROMPT_CAP);
}

#[test]
fn build_trdcrd_prompt_omits_the_research_section_when_notes_are_empty() {
    let mut p = project(ProjectPhase::Drafting);
    p.prd_markdown = "# PRD".to_string();
    let (_system, prompt) = office::build_trdcrd_prompt(&p);
    assert!(!prompt.contains("RESEARCH FINDINGS"), "no research section without notes");
}

#[test]
fn every_doc_drafting_prompt_ends_with_an_explicit_fence_reminder() {
    // NEW (design-speedup item 1: fence hardening). Recency compliance — the LAST thing the model
    // reads on a doc-drafting prompt is the exact required fence wrapper.
    let mut p = project(ProjectPhase::Drafting);
    p.prd_markdown = "# PRD".to_string();
    let (_s1, prd) = office::build_invoke(&p, "write the PRD");
    assert!(prd.contains("Reminder: your reply MUST END"), "PRD prompt carries the fence reminder");
    assert!(prd.trim_end().ends_with("```"), "PRD prompt ends on the fence wrapper");
    assert!(prd.contains("```prd"), "PRD reminder names the ```prd tag");

    let (_s2, trdcrd) = office::build_trdcrd_prompt(&p);
    assert!(trdcrd.contains("Reminder: your reply MUST END"), "TRD+CRD prompt carries the fence reminder");
    // Both fence tags appear in the trailing reminder.
    let reminder = &trdcrd[trdcrd.rfind("Reminder:").unwrap()..];
    assert!(reminder.contains("```trd") && reminder.contains("```crd"), "reminder names both tags");

    // The ask-mode auto-resolve rewrite is a revision invoke and also ends with the reminder.
    let (_s3, resolve) = office::build_assume_resolve_prompt(&p, "PRD", "# PRD", &["x".to_string()], &["prd"]);
    assert!(resolve.contains("Reminder: your reply MUST END"), "resolve prompt carries the fence reminder");
}

#[test]
fn extract_research_prefers_the_findings_block_else_the_whole_text() {
    assert_eq!(
        office::extract_research("intro\nOFFICE-RESEARCH\nfindings: - a\n- b\n"),
        "- a\n- b"
    );
    // No block -> the whole reply text, trimmed.
    assert_eq!(office::extract_research("  just prose  "), "just prose");
}

#[test]
fn extract_research_caps_at_16kb_with_a_marker() {
    let huge = format!("OFFICE-RESEARCH\nfindings: {}", "x".repeat(40_000));
    let notes = office::extract_research(&huge);
    assert!(notes.len() <= office::RESEARCH_NOTES_CAP, "notes were {} bytes", notes.len());
    assert!(notes.ends_with("... [truncated]"));
}

#[test]
fn build_breakdown_prompt_folds_the_trd_when_present() {
    let mut p = project(ProjectPhase::Drafting);
    p.trd_markdown = "# TRD\naxum 0.7".to_string();
    let (_s, normal) = office::build_breakdown_prompt(&p, None, false);
    assert!(normal.contains("axum 0.7"), "TRD folded into the normal breakdown prompt");
    let (_s2, compact) = office::build_breakdown_prompt(&p, None, true);
    assert!(compact.contains("axum 0.7"), "compact mode gets the TRD slice too");
    assert!(compact.contains("COMPACT MODE"));
}

// ---- combined TRD+CRD body + one-shot safeguard gate builders (design-speedup) ----

#[test]
fn trdcrd_body_labels_both_docs() {
    // NEW: the combined doc-set body the single TRD+CRD gate operates on.
    let mut p = project(ProjectPhase::Drafting);
    p.trd_markdown = "# TRD\naxum 0.7".to_string();
    p.crd_markdown = "# CRD\nREADME present".to_string();
    let body = office::trdcrd_body(&p);
    assert!(body.contains("Technical Requirements Document") && body.contains("axum 0.7"));
    assert!(body.contains("Clean-build Requirement Document") && body.contains("README present"));
}

#[test]
fn no_assume_clause_is_on_every_doc_contract() {
    // ADAPTED: the TRD and CRD contracts now live in the ONE combined authoring prompt.
    let p = project(ProjectPhase::Drafting);
    let (_s1, prd_prompt) = office::build_invoke(&p, "write the PRD");
    let (_s2, trdcrd_prompt) = office::build_trdcrd_prompt(&p);
    for prompt in [&prd_prompt, &trdcrd_prompt] {
        assert!(prompt.contains("Do NOT assume"), "no-assume clause present");
        assert!(prompt.contains("Open questions"), "ungrounded choices routed to Open questions");
        assert!(prompt.contains("Delegated decision"), "delegated choices are recorded, not assumed");
    }
}

#[test]
fn build_assume_check_prompt_uses_only_user_turns_and_states_the_block() {
    // ADAPTED (design-speedup one-shot gate): the enumerate builder now takes tags + resolve_inline
    // + ask_wellknown. Plain enumerate (no inline resolve, no well-known) here.
    let mut p = project(ProjectPhase::Drafting);
    p.office_transcript = vec![
        turn(ChatAuthor::User, "build a todo app"),
        turn(ChatAuthor::Office, "I assumed you want Postgres"),
    ];
    p.research_notes = "reqwest 0.12 is current".to_string();
    let (system, prompt) =
        office::build_assume_check_prompt(&p, "PRD", "# PRD\nUses Postgres.", &["prd"], false, false);
    assert!(system.contains("safeguard"), "system frames the safeguard role");
    assert!(prompt.contains("build a todo app"), "the user's own turn is ground truth");
    assert!(!prompt.contains("I assumed you want Postgres"), "the office's own reply is NOT ground truth");
    assert!(prompt.contains("reqwest 0.12"), "research notes also count as grounded");
    assert!(prompt.contains("PRD UNDER REVIEW"), "the doc under review is labelled");
    assert!(prompt.contains("ASSUME-CHECK"), "the output block contract is stated");
    assert!(prompt.contains("verdict: clean | assumptions"));
    assert!(!prompt.contains("well-known:"), "no well-known line unless ask_wellknown");
    assert!(!prompt.contains("re-emit the COMPLETE revised"), "no inline-resolve unless resolve_inline");
    assert!(prompt.len() <= office::HARD_PROMPT_CAP);
}

#[test]
fn build_assume_check_prompt_auto_mode_folds_resolve_and_well_known() {
    // NEW (design-speedup item 4 + amendment A): the compressed auto-mode PRD enumerate asks for
    // BOTH the inline resolve (revised fences) AND the well-known boolean in one invoke.
    let mut p = project(ProjectPhase::Drafting);
    p.prd_markdown = "# PRD".to_string();
    let (_system, prompt) =
        office::build_assume_check_prompt(&p, "PRD", "# PRD", &["prd"], true, true);
    assert!(prompt.contains("re-emit the COMPLETE revised"), "inline resolve instruction present");
    assert!(prompt.contains("```prd"), "the revised fence tag is named for re-emission");
    assert!(prompt.contains("well-known:"), "the well-known boolean is requested");
}

#[test]
fn build_assume_verify_prompt_reports_only_no_rewrite() {
    // NEW (design-speedup item 5c): the final verify pass may only clear or DISCLOSE — never rewrite.
    let mut p = project(ProjectPhase::Drafting);
    p.trd_markdown = "# TRD".to_string();
    let body = office::trdcrd_body(&p);
    let (system, prompt) = office::build_assume_verify_prompt(&p, "TRD+CRD", &body);
    assert!(system.contains("VERIFY"), "system frames the verify-only role");
    assert!(prompt.contains("ASSUME-CHECK"), "reuses the ASSUME-CHECK block");
    assert!(prompt.contains("no rewrite") || prompt.contains("no fenced document"), "forbids a rewrite");
    assert!(!prompt.contains("re-emit the COMPLETE revised"), "verify never resolves");
}

#[test]
fn parse_well_known_reads_yes_no_and_defaults_none() {
    // NEW (design-speedup item 4): the research-decision boolean parser is tolerant.
    assert_eq!(office::parse_well_known("ASSUME-CHECK\nverdict: clean\nwell-known: yes\n"), Some(true));
    assert_eq!(office::parse_well_known("well-known: no"), Some(false));
    assert_eq!(office::parse_well_known("Well-Known: YES please"), Some(true));
    assert_eq!(office::parse_well_known("- well-known: false"), Some(false));
    assert_eq!(office::parse_well_known("verdict: clean\n"), None, "absent -> None (run research)");
}

// ---------------------------------------------------------------------------
// Sprints (feature: sprints)
// ---------------------------------------------------------------------------

const GOOD_WITH_SPRINTS: &str = r#"
{
  "epics": [
    { "slug": "ingest", "title": "Ingest", "intent": "pull data", "stories": [
      { "slug": "fetch", "title": "Fetch", "intent": "http", "tasks": [
        { "slug": "client", "title": "HTTP client", "description": "d", "acceptance": ["compiles"], "priority": 5, "blocked_by": [] },
        { "slug": "retry", "title": "Retry logic", "description": "d", "acceptance": ["retries"], "priority": 1, "blocked_by": ["client"] }
      ]}
    ]}
  ],
  "sprints": [
    { "goal": "HTTP foundation", "tasks": ["client"] },
    { "goal": "Resilience", "tasks": ["retry"] }
  ]
}
"#;

#[test]
fn parse_and_apply_breakdown_groups_sprints_when_present() {
    let b = office::parse_breakdown(GOOD_WITH_SPRINTS).expect("valid");
    let mut p = project(ProjectPhase::Drafting);
    office::apply_breakdown(&mut p, b);
    assert_eq!(p.sprints.len(), 2);
    assert_eq!(p.sprints[0].goal, "HTTP foundation");
    assert_eq!(p.sprints[0].tasks, vec![office_core_task_id("shop/ingest/fetch/client")]);
    assert_eq!(p.sprints[1].tasks, vec![office_core_task_id("shop/ingest/fetch/retry")]);
    assert_eq!(p.sprints[0].status, crate::domain::SprintStatus::Active, "first sprint active");
    assert_eq!(p.sprints[1].status, crate::domain::SprintStatus::Pending, "the rest pending");
}

#[test]
fn breakdown_without_sprints_yields_one_all_tasks_sprint() {
    // GOOD carries no "sprints" array -> a single implicit sprint of every task (back-compat).
    let b = office::parse_breakdown(GOOD).expect("valid");
    let mut p = project(ProjectPhase::Drafting);
    office::apply_breakdown(&mut p, b);
    assert_eq!(p.sprints.len(), 1);
    assert_eq!(p.sprints[0].goal, office::DEFAULT_SPRINT_GOAL);
    assert_eq!(p.sprints[0].tasks.len(), 2, "both tasks land in the single sprint");
    assert_eq!(p.sprints[0].status, crate::domain::SprintStatus::Active);
}

#[test]
fn breakdown_garbage_sprints_fall_back_to_single_sprint() {
    // Sprints referencing an unknown slug + an empty sprint -> all dropped -> single all-tasks sprint.
    let json = r#"{"epics":[{"slug":"e","stories":[{"slug":"s","tasks":[
      {"slug":"a","acceptance":["x"]},
      {"slug":"b","acceptance":["y"]}
    ]}]}],"sprints":[{"goal":"ghost","tasks":["does-not-exist"]},{"goal":"empty","tasks":[]}]}"#;
    let b = office::parse_breakdown(json).expect("valid");
    let mut p = project(ProjectPhase::Drafting);
    office::apply_breakdown(&mut p, b);
    assert_eq!(p.sprints.len(), 1, "no usable sprint survived -> single fallback");
    assert_eq!(p.sprints[0].tasks.len(), 2);
}

#[test]
fn breakdown_leftover_tasks_append_to_last_sprint() {
    // Only 'a' is placed; 'b' is orphaned -> appended to the last (only) surviving sprint.
    let json = r#"{"epics":[{"slug":"e","stories":[{"slug":"s","tasks":[
      {"slug":"a","acceptance":["x"]},
      {"slug":"b","acceptance":["y"]}
    ]}]}],"sprints":[{"goal":"first","tasks":["a"]}]}"#;
    let b = office::parse_breakdown(json).expect("valid");
    let mut p = project(ProjectPhase::Drafting);
    office::apply_breakdown(&mut p, b);
    assert_eq!(p.sprints.len(), 1);
    assert_eq!(p.sprints[0].tasks.len(), 2, "the orphaned task was folded into the last sprint");
}

#[test]
fn enhancement_breakdown_lands_a_single_implicit_sprint() {
    // Enhancement track: the breakdown asks for no sprints, so apply wraps its tasks in ONE sprint.
    let mut p = project(ProjectPhase::Drafting);
    p.track = "enhancement".to_string();
    let b = office::parse_breakdown(GOOD).expect("valid"); // GOOD carries no sprint grouping
    office::apply_breakdown(&mut p, b);
    assert_eq!(p.sprints.len(), 1, "enhancement = exactly one implicit sprint");
    assert_eq!(p.sprints[0].status, crate::domain::SprintStatus::Active);
    assert_eq!(p.sprints[0].tasks.len(), 2);
}

#[test]
fn breakdown_prompt_requests_sprints_on_project_track_only() {
    let mut p = project(ProjectPhase::Drafting);
    let (_s, prompt) = office::build_breakdown_prompt(&p, None, false);
    assert!(prompt.contains("\"sprints\""), "project-track breakdown asks for a sprints array");
    assert!(prompt.contains("group the tasks into ordered SPRINTS"));

    p.track = "enhancement".to_string();
    let (_s, e_prompt) = office::build_breakdown_prompt(&p, None, false);
    assert!(!e_prompt.contains("group the tasks into ordered SPRINTS"), "enhancement is one implicit sprint");

    p.track = "project".to_string();
    let (_s, c_prompt) = office::build_breakdown_prompt(&p, None, true);
    assert!(!c_prompt.contains("group the tasks into ordered SPRINTS"), "compact keeps the ask tiny");
}

#[test]
fn sprint_review_prompt_renders_transcript_and_next_sprint() {
    let p = project(ProjectPhase::Running);
    let transcript = vec![
        crate::domain::SprintLine { speaker: "nova".to_string(), line: "built the client".to_string() },
        crate::domain::SprintLine { speaker: "reviewer".to_string(), line: "1 passed".to_string() },
    ];
    let next_tasks = vec![("shop/x/y/z".to_string(), "Next task".to_string())];
    let (_s, prompt) = office::build_sprint_review_prompt(
        &p,
        "Ship it",
        &transcript,
        &["carried".to_string()],
        Some(("Next goal", &next_tasks)),
    );
    assert!(prompt.contains("nova: built the client"));
    assert!(prompt.contains("SPRINT-REVIEW"));
    assert!(prompt.contains("Next goal") && prompt.contains("[shop/x/y/z]"));
    assert!(prompt.contains("CARRY-OVER"));

    // A last sprint (no next) -> summary only, no adjustments block offered.
    let (_s2, last) = office::build_sprint_review_prompt(&p, "Ship it", &transcript, &[], None);
    assert!(last.contains("LAST sprint"));
    assert!(!last.contains("adjustments:"));
}

#[test]
fn append_research_learnings_keeps_newest_within_cap() {
    let existing = "old stack notes";
    let out = office::append_research_learnings(existing, "sprint 1 learnings");
    assert!(out.starts_with("sprint 1 learnings"), "newest learnings prepended");
    assert!(out.contains("old stack notes"));
    // An empty addition is a no-op.
    assert_eq!(office::append_research_learnings(existing, "   "), existing);
    // Over-cap keeps the newest head, drops the oldest tail.
    let huge = "x".repeat(office::RESEARCH_NOTES_CAP);
    let capped = office::append_research_learnings(&huge, "FRESH");
    assert!(capped.starts_with("FRESH"));
    assert!(capped.len() <= office::RESEARCH_NOTES_CAP);
}

#[test]
fn append_research_learnings_reserves_floor_for_original_research() {
    // Review finding (MINOR, eviction floor): repeated large sprint-learnings appends must never
    // evict the ORIGINAL stack research entirely. The accumulated learnings prefix is capped at half
    // the budget, so the original research (found here via a distinctive marker near its start)
    // always survives, however many verbose sprints pile learnings on top of it.
    let original = format!("ORIGINAL-STACK-RESEARCH-MARKER {}", "y".repeat(2000));
    let mut notes = original.clone();
    let big_learning = "z".repeat(office::RESEARCH_NOTES_CAP); // one huge sprint's learnings
    for _ in 0..10 {
        notes = office::append_research_learnings(&notes, &big_learning);
        assert!(notes.len() <= office::RESEARCH_NOTES_CAP, "stays within the overall cap");
    }
    assert!(
        notes.contains("ORIGINAL-STACK-RESEARCH-MARKER"),
        "the original research must survive even after many large learnings appends"
    );
}
