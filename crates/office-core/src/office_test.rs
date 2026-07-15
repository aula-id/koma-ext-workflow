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
        office_transcript: vec![],
        office_summary: String::new(),
        delivery_path: None,
        bound_session: Some("sess-x".to_string()),
        workspace: Some(PathBuf::from("/ws")),
        epics: vec![],
        stories: vec![],
        tasks: vec![],
        config: ProjectConfig::default_config(),
        outbox: vec![],
        seq: 1,
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
fn build_trd_prompt_folds_prd_and_research_notes_with_the_trd_contract() {
    let mut p = project(ProjectPhase::Drafting);
    p.prd_markdown = "# PRD\nCrawler.".to_string();
    p.research_notes = "use reqwest 0.12".to_string();
    let (system, prompt) = office::build_trd_prompt(&p);
    assert!(system.contains("front office"));
    assert!(prompt.contains("Crawler"), "PRD is included");
    assert!(prompt.contains("reqwest 0.12"), "research notes are included when present");
    assert!(prompt.contains("```trd"), "the ```trd capture contract is stated");
    assert!(prompt.len() <= office::HARD_PROMPT_CAP);
}

#[test]
fn build_trd_prompt_omits_the_research_section_when_notes_are_empty() {
    let mut p = project(ProjectPhase::Drafting);
    p.prd_markdown = "# PRD".to_string();
    let (_system, prompt) = office::build_trd_prompt(&p);
    assert!(!prompt.contains("RESEARCH FINDINGS"), "no research section without notes");
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

// ---- CRD + safeguard prompt builders (6.2c) ----

#[test]
fn build_crd_prompt_folds_prd_and_trd_with_the_crd_contract_and_rubric() {
    let mut p = project(ProjectPhase::Drafting);
    p.prd_markdown = "# PRD\nCrawler.".to_string();
    p.trd_markdown = "# TRD\nUse axum 0.7".to_string();
    let (system, prompt) = office::build_crd_prompt(&p);
    assert!(system.contains("front office"));
    assert!(prompt.contains("Crawler"), "PRD is folded in");
    assert!(prompt.contains("axum 0.7"), "TRD is folded in when present");
    assert!(prompt.contains("```crd"), "the ```crd capture contract is stated");
    assert!(prompt.contains("rubric") || prompt.contains("Grading rubric"), "asks for a grading rubric");
    assert!(prompt.contains("SUM TO EXACTLY 100"), "rubric weights must total 100");
    assert!(prompt.len() <= office::HARD_PROMPT_CAP);
}

#[test]
fn build_crd_prompt_omits_trd_section_when_absent() {
    let mut p = project(ProjectPhase::Drafting);
    p.prd_markdown = "# PRD".to_string();
    let (_s, prompt) = office::build_crd_prompt(&p);
    assert!(!prompt.contains("TRD (technical requirements"), "no TRD section without a TRD");
}

#[test]
fn no_assume_clause_is_on_every_doc_contract() {
    let p = project(ProjectPhase::Drafting);
    // The PRD persona contract, the TRD prompt, and the CRD prompt all carry the no-assume gate.
    let (_s1, prd_prompt) = office::build_invoke(&p, "write the PRD");
    let (_s2, trd_prompt) = office::build_trd_prompt(&p);
    let (_s3, crd_prompt) = office::build_crd_prompt(&p);
    for prompt in [&prd_prompt, &trd_prompt, &crd_prompt] {
        assert!(prompt.contains("Do NOT assume"), "no-assume clause present");
        assert!(prompt.contains("Open questions"), "ungrounded choices routed to Open questions");
        assert!(prompt.contains("Delegated decision"), "delegated choices are recorded, not assumed");
    }
}

#[test]
fn build_assume_check_prompt_uses_only_user_turns_and_states_the_block() {
    let mut p = project(ProjectPhase::Drafting);
    p.office_transcript = vec![
        turn(ChatAuthor::User, "build a todo app"),
        turn(ChatAuthor::Office, "I assumed you want Postgres"),
    ];
    p.research_notes = "reqwest 0.12 is current".to_string();
    let (system, prompt) = office::build_assume_check_prompt(&p, "PRD", "# PRD\nUses Postgres.");
    assert!(system.contains("safeguard"), "system frames the safeguard role");
    assert!(prompt.contains("build a todo app"), "the user's own turn is ground truth");
    assert!(!prompt.contains("I assumed you want Postgres"), "the office's own reply is NOT ground truth");
    assert!(prompt.contains("reqwest 0.12"), "research notes also count as grounded");
    assert!(prompt.contains("PRD UNDER REVIEW"), "the doc under review is labelled");
    assert!(prompt.contains("ASSUME-CHECK"), "the output block contract is stated");
    assert!(prompt.contains("verdict: clean | assumptions"));
    assert!(prompt.len() <= office::HARD_PROMPT_CAP);
}
