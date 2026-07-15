#[cfg(test)]
mod tests {
    use crate::digest::{context_blob, panel_snapshot, SnapshotMode, CONTEXT_BLOB_CAP};
    use crate::domain::*;

    fn task(id: &str, state: TaskState) -> Task {
        Task {
            id: TaskId(id.to_string()),
            title: format!("Task {id}"),
            description: "do the thing".to_string(),
            acceptance: vec!["works".to_string()],
            blocked_by: Vec::new(),
            priority: 0,
            state,
            bounces: 0,
            comments: vec![Comment {
                id: CommentId(1),
                author: CommentAuthor::User,
                text: "please hurry".to_string(),
                created_ms: 1,
                receipt: Receipt::Pending,
            }],
            desk: None,
            last_report: Some("worker report body".to_string()),
            last_review: Some("reviewer verdict body".to_string()),
            history: vec![TaskEvent { at_ms: 1, event: "created".to_string() }],
        }
    }

    fn project(id: &str, seq: u64, phase: ProjectPhase, tasks: Vec<Task>) -> Project {
        Project {
            id: ProjectId(id.to_string()),
            name: format!("Project {id}"),
            phase,
            prd_markdown: "# PRD\nbody".to_string(),
            trd_markdown: String::new(),
            research_notes: String::new(),
            research: None,
            office_transcript: vec![ChatMsg { who: ChatAuthor::User, text: "hi".to_string() }],
            office_summary: String::new(),
            delivery_path: Some(format!("/work/{id}/deliver").into()),
            bound_session: Some("s1".to_string()),
            workspace: Some("/work".into()),
            epics: Vec::new(),
            stories: Vec::new(),
            tasks,
            config: ProjectConfig::default_config(),
            outbox: Vec::new(),
            seq,
        }
    }

    #[test]
    fn context_blob_stays_under_cap_with_many_projects() {
        let projects: Vec<Project> = (0..200)
            .map(|i| {
                project(
                    &format!("proj-{i}"),
                    i as u64,
                    ProjectPhase::Running,
                    vec![task("t1", TaskState::Todo), task("t2", TaskState::Done { at_ms: 1 })],
                )
            })
            .collect();

        let blob = context_blob(&projects);
        assert!(blob.len() <= CONTEXT_BLOB_CAP, "blob was {} bytes", blob.len());
    }

    #[test]
    fn context_blob_instruction_block_always_present() {
        let projects = vec![project("p1", 1, ProjectPhase::Running, vec![])];
        let blob = context_blob(&projects);
        assert!(blob.starts_with("# Workflow\n"));
        assert!(blob.contains("Active projects: 1."));
        assert!(blob.contains("koma-workflow/inbox/<millis>-<slug>.json"));
        assert!(blob.contains("\"op\":\"brief\""));
    }

    #[test]
    fn context_blob_instruction_block_survives_even_with_huge_project_count() {
        // Force a scenario where projects definitely get dropped, and assert the
        // footer (never-truncated instruction block) is still intact.
        let projects: Vec<Project> = (0..500)
            .map(|i| {
                project(
                    &format!("very-long-project-slug-{i}-with-extra-padding-characters"),
                    i as u64,
                    ProjectPhase::Halted { reason: "x".repeat(200) },
                    vec![task("t1", TaskState::Parked { reason: ParkReason::ReviewBounceBudget, attempt: 1 })],
                )
            })
            .collect();

        let blob = context_blob(&projects);
        assert!(blob.len() <= CONTEXT_BLOB_CAP);
        assert!(blob.ends_with(
            "{\"op\":\"brief\",\"project\":\"<id>\",\"message\":\"...\"} (ops: brief,status,authorize,interrupt,resume,comment)\n"
        ));
        // Not every project fit.
        assert!(blob.matches("- very-long-project-slug-").count() < 500);
    }

    #[test]
    fn context_blob_most_recently_active_first() {
        let projects = vec![
            project("old", 1, ProjectPhase::Running, vec![]),
            project("new", 99, ProjectPhase::Running, vec![]),
        ];
        let blob = context_blob(&projects);
        let idx_old = blob.find("- old:").expect("old present");
        let idx_new = blob.find("- new:").expect("new present");
        assert!(idx_new < idx_old, "higher-seq project should render first");
    }

    #[test]
    fn context_blob_attention_line_surfaces_halt_and_park() {
        let projects = vec![project(
            "p1",
            1,
            ProjectPhase::Halted { reason: "t4 blocks everything".to_string() },
            vec![task("t4", TaskState::Parked { reason: ParkReason::WorkerBlocked("needs creds".to_string()), attempt: 1 })],
        )];
        let blob = context_blob(&projects);
        assert!(blob.contains("attention:"));
        assert!(blob.contains("halted: t4 blocks everything"));
    }

    #[test]
    fn panel_snapshot_full_mode_includes_report_and_history() {
        let projects = vec![project("p1", 1, ProjectPhase::Running, vec![task("t1", TaskState::Todo)])];
        let snap = panel_snapshot(&projects, SnapshotMode::Full);
        let arr = snap.as_array().expect("array");
        let t = &arr[0]["tasks"][0];
        assert_eq!(t["lastReport"], "worker report body");
        assert_eq!(t["lastReview"], "reviewer verdict body");
        assert_eq!(t["history"].as_array().unwrap().len(), 1);
        assert_eq!(t["comments"].as_array().unwrap().len(), 1);
        assert_eq!(arr[0]["prdMarkdown"], "# PRD\nbody");
    }

    #[test]
    fn panel_snapshot_full_mode_round_trips_config_including_keep_desks() {
        let mut p = project("p1", 1, ProjectPhase::Running, vec![]);
        p.config.max_workers = 3;
        p.config.bounce_budget = 5;
        p.config.worker_model = Some("gpt-5".to_string());
        p.config.reviewer_model = None;
        p.config.keep_desks = true;
        let snap = panel_snapshot(&[p], SnapshotMode::Full);
        let cfg = &snap.as_array().expect("array")[0]["config"];
        assert_eq!(cfg["maxWorkers"], 3);
        assert_eq!(cfg["bounceBudget"], 5);
        assert_eq!(cfg["workerModel"], "gpt-5");
        assert!(cfg["reviewerModel"].is_null());
        assert_eq!(cfg["keepDesks"], true);
    }

    #[test]
    fn panel_snapshot_summary_mode_drops_report_and_history_bodies() {
        let projects = vec![project("p1", 1, ProjectPhase::Running, vec![task("t1", TaskState::Todo)])];
        let snap = panel_snapshot(&projects, SnapshotMode::Summary);
        let arr = snap.as_array().expect("array");
        let t = &arr[0]["tasks"][0];

        assert!(t.get("lastReport").is_none());
        assert!(t.get("lastReview").is_none());
        assert!(t.get("history").is_none());
        assert!(t.get("comments").is_none());
        assert!(t.get("description").is_none());
        assert!(arr[0].get("prdMarkdown").is_none());
        assert!(arr[0].get("trdMarkdown").is_none(), "summary mode omits the TRD body");
        assert!(arr[0].get("researchNotes").is_none(), "summary mode omits research notes");
        assert!(arr[0].get("config").is_none(), "summary mode omits config too (size guard)");

        // Counts and state survive.
        assert_eq!(t["id"], "t1");
        assert_eq!(t["state"], "todo");
        assert_eq!(t["column"], "todo");
    }

    #[test]
    fn panel_snapshot_full_mode_carries_trd_and_research_notes() {
        let mut p = project("p1", 1, ProjectPhase::Running, vec![]);
        p.trd_markdown = "# TRD\naxum 0.7".to_string();
        p.research_notes = "- reqwest 0.12 for HTTP".to_string();
        let snap = panel_snapshot(&[p], SnapshotMode::Full);
        let obj = &snap.as_array().expect("array")[0];
        assert_eq!(obj["trdMarkdown"], "# TRD\naxum 0.7");
        assert_eq!(obj["researchNotes"], "- reqwest 0.12 for HTTP");
    }
}
