#[cfg(test)]
mod tests {
    use crate::domain::*;
    use crate::prompts::{office_system, reviewer, worker, PROMPT_TARGET_CAP};
    use std::path::Path;

    fn base_project() -> Project {
        Project {
            id: ProjectId("shop-crawler".to_string()),
            name: "Shop Crawler".to_string(),
            phase: ProjectPhase::Running,
            prd_markdown: "Crawl shop listings and extract prices.\nMore detail below.".to_string(),
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
            delivery_path: Some("/work/session/deliver".into()),
            bound_session: Some("session-1".to_string()),
            workspace: Some("/work/session".into()),
            epics: vec![Epic {
                id: EpicId("shop-crawler/e1-ingest".to_string()),
                title: "Ingest".to_string(),
                intent: "Pull raw listing pages".to_string(),
                stories: vec![StoryId("shop-crawler/e1-ingest/s2-parser".to_string())],
            }],
            stories: vec![Story {
                id: StoryId("shop-crawler/e1-ingest/s2-parser".to_string()),
                title: "Parser".to_string(),
                intent: "Turn HTML into structured rows".to_string(),
                tasks: vec![TaskId("shop-crawler/e1-ingest/s2-parser/t4-retry-logic".to_string())],
            }],
            tasks: vec![base_task()],
            config: ProjectConfig::default_config(),
            outbox: Vec::new(),
            trace: Vec::new(),
            interrupted_from: None,
            gate_cleared: false,
            gate_invoke_live_hint: false,
            pending_breakdown: None,
            seq: 1,
            worktree_desks: false,
            workflow_home: None,
            hygiene_sum: 0,
            hygiene_count: 0,
        }
    }

    fn base_task() -> Task {
        Task {
            id: TaskId("shop-crawler/e1-ingest/s2-parser/t4-retry-logic".to_string()),
            title: "Retry logic".to_string(),
            description: "Add exponential backoff to the fetcher.".to_string(),
            acceptance: vec!["Retries 3 times".to_string(), "Backoff is exponential".to_string()],
            blocked_by: Vec::new(),
            priority: 1,
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
        }
    }

    fn comment(id: u64, text: &str) -> Comment {
        Comment {
            id: CommentId(id),
            author: CommentAuthor::User,
            text: text.to_string(),
            created_ms: 1000,
            receipt: Receipt::Pending,
        }
    }

    #[test]
    fn worker_prompt_has_required_sections() {
        let project = base_project();
        let task = &project.tasks[0];
        let prompt = worker(
            &project,
            task,
            Path::new("/work/session/koma-workflow/desks/shop-crawler/t4--koma-workflow-desk"),
            Path::new("/work/session/deliver"),
            1,
            None,
            &[],
        );

        assert!(prompt.contains("PROJECT: Shop Crawler"));
        assert!(prompt.contains("EPIC: Ingest"));
        assert!(prompt.contains("STORY: Parser"));
        assert!(prompt.contains("TASK shop-crawler/e1-ingest/s2-parser/t4-retry-logic"));
        assert!(prompt.contains("ACCEPTANCE CRITERIA"));
        assert!(prompt.contains("Retries 3 times"));
        assert!(prompt.contains("WORKSPACE RULES"));
        assert!(prompt.contains("koma-workflow-desk"));
        assert!(prompt.contains("Deliverables go ONLY to"));
        assert!(prompt.contains("OFFICE-REPORT"));
        assert!(prompt.contains("status: complete | blocked"));
        assert!(prompt.contains("ack-comments:"));

        // No prior attempts on a first attempt with no review notes.
        assert!(!prompt.contains("PRIOR ATTEMPTS"));
        assert!(!prompt.contains("COMMENTS FROM THE BOARD"));

        assert!(prompt.len() < PROMPT_TARGET_CAP, "prompt was {} bytes", prompt.len());
    }

    #[test]
    fn worker_prompt_renders_prior_attempt_review_notes() {
        let project = base_project();
        let task = &project.tasks[0];
        let prompt = worker(
            &project,
            task,
            Path::new("/work/session/koma-workflow/desks/shop-crawler/t4--koma-workflow-desk"),
            Path::new("/work/session/deliver"),
            2,
            Some("Backoff was linear, not exponential."),
            &[],
        );

        assert!(prompt.contains("PRIOR ATTEMPTS"));
        assert!(prompt.contains("Backoff was linear, not exponential."));
    }

    #[test]
    fn worker_prompt_ack_instruction_carries_comment_ids() {
        let project = base_project();
        let task = &project.tasks[0];
        let comments = vec![comment(17, "please also handle 429s"), comment(18, "and log the retry count")];
        let prompt = worker(
            &project,
            task,
            Path::new("/desk"),
            Path::new("/deliver"),
            1,
            None,
            &comments,
        );

        assert!(prompt.contains("COMMENTS FROM THE BOARD"));
        assert!(prompt.contains("[c17]"));
        assert!(prompt.contains("please also handle 429s"));
        assert!(prompt.contains("[c18]"));
        assert!(prompt.contains("and log the retry count"));
    }

    #[test]
    fn worker_prompt_truncates_oversized_variable_sections_and_stays_under_cap() {
        let mut project = base_project();
        // Oversized description, review notes, acceptance list, and comment texts.
        project.tasks[0].description = "d".repeat(50_000);
        project.tasks[0].acceptance = (0..500).map(|i| format!("criterion {i} {}", "x".repeat(500))).collect();
        let task = project.tasks[0].clone();

        let huge_notes = "n".repeat(50_000);
        let comments: Vec<Comment> = (0..200)
            .map(|i| comment(i, &format!("comment {i} {}", "y".repeat(1000))))
            .collect();

        let prompt = worker(
            &project,
            &task,
            Path::new("/desk"),
            Path::new("/deliver"),
            5,
            Some(&huge_notes),
            &comments,
        );

        assert!(
            prompt.len() < PROMPT_TARGET_CAP,
            "oversized-input prompt was {} bytes, expected < {}",
            prompt.len(),
            PROMPT_TARGET_CAP
        );
        // The fixed protocol block must survive truncation intact.
        assert!(prompt.contains("OFFICE-REPORT"));
        assert!(prompt.contains("status: complete | blocked"));
    }

    #[test]
    fn reviewer_prompt_has_required_sections() {
        let project = base_project();
        let task = &project.tasks[0];
        let prompt = reviewer(
            &project,
            task,
            Path::new("/work/session/deliver"),
            "Implemented retry with exponential backoff.",
            &["/work/session/deliver/fetcher.rs".to_string()],
        );

        assert!(prompt.contains("You are a Workflow reviewer"));
        assert!(prompt.contains("TASK shop-crawler/e1-ingest/s2-parser/t4-retry-logic"));
        assert!(prompt.contains("Retries 3 times"));
        assert!(prompt.contains("WORKER SUMMARY"));
        assert!(prompt.contains("Implemented retry with exponential backoff."));
        assert!(prompt.contains("fetcher.rs"));
        assert!(prompt.contains("OFFICE-REVIEW"));
        assert!(prompt.contains("verdict: pass | fail"));
        // item 2/3: the board digest, the hygiene gate, and the optional hygiene grade line are in
        // EVERY reviewer prompt (legacy included) — tree hygiene is now per-task owned.
        assert!(prompt.contains("BOARD DIGEST"));
        assert!(prompt.contains("CLEAN-BUILD HYGIENE"));
        assert!(prompt.contains("hygiene: <0-100"));
        assert!(prompt.len() < PROMPT_TARGET_CAP);
    }

    #[test]
    fn reviewer_prompt_worktree_shows_diff_stat_and_board_digest() {
        // item 2: in worktree mode the reviewer reviews the INTEGRATED task worktree + the branch
        // diff-stat, and the board digest names merged + in-flight siblings (with owners).
        let mut project = base_project();
        project.worktree_desks = true;
        project.workflow_home = Some("/home/.koma-workflow".into());
        project.tasks[0].desk = Some("/home/.koma-workflow/desks/shop-crawler/t4-retry-logic".into());
        project.tasks[0].diff_stat = Some(" fetcher.rs | 12 ++++++++----\n 1 file changed".to_string());

        let mut merged = base_task();
        merged.id = TaskId("shop-crawler/e1-ingest/s2-parser/t1-http".to_string());
        merged.title = "HTTP client".to_string();
        merged.state = TaskState::Done { at_ms: 10 };
        let mut inflight = base_task();
        inflight.id = TaskId("shop-crawler/e1-ingest/s2-parser/t9-cache".to_string());
        inflight.title = "Cache layer".to_string();
        inflight.state = TaskState::OnProgress {
            binding: AgentBinding {
                ext_agent_id: 5,
                session: "s".to_string(),
                spawned_at_ms: 0,
                kind: AgentKind::Worker,
                persona: "office-worker-nova".to_string(),
            },
            attempt: 1,
        };
        project.tasks.push(merged);
        project.tasks.push(inflight);

        let task = &project.tasks[0].clone();
        let prompt = reviewer(&project, task, Path::new("/work/session/deliver"), "did it", &[]);

        assert!(prompt.contains("git worktree at /home/.koma-workflow/desks/shop-crawler/t4-retry-logic"));
        assert!(prompt.contains("TASK BRANCH DIFF vs main"));
        assert!(prompt.contains("fetcher.rs | 12"));
        assert!(prompt.contains("BOARD DIGEST"));
        assert!(prompt.contains("t1-http")); // merged sibling
        assert!(prompt.contains("t9-cache")); // in-flight sibling
        assert!(prompt.contains("nova")); // owner persona
        assert!(prompt.contains("CLEAN-BUILD HYGIENE"));
        assert!(prompt.len() < PROMPT_TARGET_CAP);
    }

    #[test]
    fn worker_prompt_worktree_bans_git_and_makes_the_tree_the_deliverable() {
        // item 1/2: worktree wording — the desk IS the tree, no copy step, and git is off-limits.
        let mut project = base_project();
        project.worktree_desks = true;
        let task = &project.tasks[0].clone();
        let prompt = worker(
            &project,
            task,
            Path::new("/home/.koma-workflow/desks/shop-crawler/t4"),
            Path::new("/work/session/deliver"),
            1,
            None,
            &[],
        );
        assert!(prompt.contains("full git worktree"));
        assert!(prompt.contains("NEVER run git"));
        assert!(prompt.contains("ARE the deliverables"));
        assert!(prompt.contains("CLEAN-BUILD HYGIENE"));
        // The legacy "deliver elsewhere" line is gone in worktree mode.
        assert!(!prompt.contains("Deliverables go ONLY to"));
    }

    #[test]
    fn worker_prompt_legacy_bans_git_but_keeps_delivery_path() {
        // item 1: even the legacy copy-desk prompt now forbids git (the office owns VCS).
        let project = base_project(); // worktree_desks = false
        let task = &project.tasks[0].clone();
        let prompt = worker(&project, task, Path::new("/desk"), Path::new("/deliver"), 1, None, &[]);
        assert!(prompt.contains("NEVER run git"));
        assert!(prompt.contains("Deliverables go ONLY to"));
        assert!(prompt.contains("CLEAN-BUILD HYGIENE"));
    }

    #[test]
    fn reviewer_prompt_truncates_oversized_summary() {
        let project = base_project();
        let task = &project.tasks[0];
        let huge_summary = "s".repeat(50_000);
        let delivered: Vec<String> = (0..500).map(|i| format!("/deliver/file-{i}.rs")).collect();

        let prompt = reviewer(&project, task, Path::new("/deliver"), &huge_summary, &delivered);

        assert!(prompt.len() < PROMPT_TARGET_CAP, "prompt was {} bytes", prompt.len());
        assert!(prompt.contains("OFFICE-REVIEW"));
    }

    #[test]
    fn office_system_includes_persona_and_digest() {
        let system = office_system("# Workflow\nActive projects: 1.\n");
        assert!(system.contains("front office"));
        assert!(system.contains("senior delivery manager"));
        assert!(system.contains("# Workflow"));
        assert!(system.contains("Active projects: 1."));
    }
}
