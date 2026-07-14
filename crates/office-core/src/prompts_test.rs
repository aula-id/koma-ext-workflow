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
            seq: 1,
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
        assert!(prompt.len() < PROMPT_TARGET_CAP);
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
