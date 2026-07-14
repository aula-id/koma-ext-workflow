#[cfg(test)]
mod tests {
    use crate::*;

    #[test]
    fn test_column_projection() {
        assert_eq!(column(&TaskState::Backlog), Column::Backlog);
        assert_eq!(column(&TaskState::Todo), Column::Todo);
        assert_eq!(
            column(&TaskState::OnProgress {
                binding: AgentBinding {
                    ext_agent_id: 1,
                    session: "session-1".to_string(),
                    spawned_at_ms: 1000,
                    kind: AgentKind::Worker,
                },
                attempt: 1,
            }),
            Column::OnProgress
        );
        assert_eq!(
            column(&TaskState::Review {
                binding: None,
                attempt: 1,
            }),
            Column::Review
        );
        assert_eq!(
            column(&TaskState::Parked {
                reason: ParkReason::ReviewBounceBudget,
                attempt: 1,
            }),
            Column::Review
        );
        assert_eq!(
            column(&TaskState::Done { at_ms: 2000 }),
            Column::Done
        );
    }

    #[test]
    fn test_slug_minting() {
        // Valid slugs with lowercase and digits only
        assert!(mint_id("shop-crawler").is_ok());
        assert!(mint_id("task-123").is_ok());
        assert!(mint_id("a").is_ok());
        assert_eq!(mint_id("shop-crawler").unwrap(), "shop-crawler");

        // Invalid slugs with uppercase or special characters
        assert!(mint_id("ShopCrawler").is_err());
        assert!(mint_id("task_123").is_err());
        assert!(mint_id("task@123").is_err());
        assert!(mint_id("").is_err());
    }

    #[test]
    fn test_slug_minting_with_suffix() {
        let id = mint_id_with_suffix("shop-crawler", 3).unwrap();
        assert_eq!(id, "shop-crawler-3");

        let id = mint_id_with_suffix("task-123", 1).unwrap();
        assert_eq!(id, "task-123-1");

        // Invalid base slug should error
        assert!(mint_id_with_suffix("InvalidSlug", 1).is_err());
    }

    #[test]
    fn test_full_project_serde_roundtrip() {
        let now = 1000u64;

        // Create a fully-populated project with every enum variant instantiated
        let project = Project {
            id: ProjectId("shop-crawler".to_string()),
            name: "Shop Crawler Project".to_string(),
            phase: ProjectPhase::Running,
            prd_markdown: "# Shop Crawler\nA project to crawl web shops.".to_string(),
            office_transcript: vec![
                ChatMsg {
                    who: ChatAuthor::User,
                    text: "Build a shop crawler".to_string(),
                },
                ChatMsg {
                    who: ChatAuthor::Office,
                    text: "I'll help you with that.".to_string(),
                },
            ],
            office_summary: "Building a shop crawler to ingest product data.".to_string(),
            delivery_path: Some("/tmp/delivery".into()),
            bound_session: Some("session-123".to_string()),
            workspace: Some("/home/user/project".into()),
            epics: vec![Epic {
                id: EpicId("shop-crawler/e1-ingest".to_string()),
                title: "Data Ingestion".to_string(),
                intent: "Ingest product data from shops".to_string(),
                stories: vec![StoryId("shop-crawler/e1-ingest/s1-parse".to_string())],
            }],
            stories: vec![Story {
                id: StoryId("shop-crawler/e1-ingest/s1-parse".to_string()),
                title: "Parse Shop Data".to_string(),
                intent: "Parse JSON product listings".to_string(),
                tasks: vec![TaskId("shop-crawler/e1-ingest/s1-parse/t1-http".to_string())],
            }],
            tasks: vec![
                // Task in Backlog state
                Task {
                    id: TaskId("shop-crawler/e1-ingest/s1-parse/t1-http".to_string()),
                    title: "HTTP Fetcher".to_string(),
                    description: "Implement HTTP client for fetching shop pages".to_string(),
                    acceptance: vec![
                        "Fetches pages from multiple shops".to_string(),
                        "Handles timeouts gracefully".to_string(),
                    ],
                    blocked_by: vec![],
                    priority: 100,
                    state: TaskState::Backlog,
                    bounces: 0,
                    comments: vec![],
                    desk: None,
                    last_report: None,
                    last_review: None,
                    history: vec![TaskEvent {
                        at_ms: now,
                        event: "Created".to_string(),
                    }],
                },
                // Task in Todo state
                Task {
                    id: TaskId("shop-crawler/e1-ingest/s1-parse/t2-regex".to_string()),
                    title: "Regex Parser".to_string(),
                    description: "Parse product info with regex".to_string(),
                    acceptance: vec!["Matches all product formats".to_string()],
                    blocked_by: vec![TaskId("shop-crawler/e1-ingest/s1-parse/t1-http".to_string())],
                    priority: 90,
                    state: TaskState::Todo,
                    bounces: 0,
                    comments: vec![Comment {
                        id: CommentId(1),
                        author: CommentAuthor::User,
                        text: "Make sure to handle edge cases".to_string(),
                        created_ms: now + 100,
                        receipt: Receipt::Pending,
                    }],
                    desk: None,
                    last_report: None,
                    last_review: None,
                    history: vec![],
                },
                // Task in OnProgress state
                Task {
                    id: TaskId("shop-crawler/e1-ingest/s1-parse/t3-json".to_string()),
                    title: "JSON Parser".to_string(),
                    description: "Parse JSON responses".to_string(),
                    acceptance: vec!["Parses all JSON formats".to_string()],
                    blocked_by: vec![],
                    priority: 80,
                    state: TaskState::OnProgress {
                        binding: AgentBinding {
                            ext_agent_id: 42,
                            session: "session-123".to_string(),
                            spawned_at_ms: now + 200,
                            kind: AgentKind::Worker,
                        },
                        attempt: 1,
                    },
                    bounces: 0,
                    comments: vec![],
                    desk: Some("/tmp/desk/t3".into()),
                    last_report: None,
                    last_review: None,
                    history: vec![],
                },
                // Task in Review state with binding
                Task {
                    id: TaskId("shop-crawler/e1-ingest/s1-parse/t4-retry".to_string()),
                    title: "Retry Logic".to_string(),
                    description: "Add exponential backoff".to_string(),
                    acceptance: vec!["Retries with backoff".to_string()],
                    blocked_by: vec![],
                    priority: 70,
                    state: TaskState::Review {
                        binding: Some(AgentBinding {
                            ext_agent_id: 43,
                            session: "session-123".to_string(),
                            spawned_at_ms: now + 300,
                            kind: AgentKind::Reviewer,
                        }),
                        attempt: 1,
                    },
                    bounces: 0,
                    comments: vec![],
                    desk: Some("/tmp/desk/t4".into()),
                    last_report: Some("Implemented basic retry".to_string()),
                    last_review: None,
                    history: vec![],
                },
                // Task in Review state without binding
                Task {
                    id: TaskId("shop-crawler/e1-ingest/s1-parse/t5-cache".to_string()),
                    title: "Cache Layer".to_string(),
                    description: "Add response caching".to_string(),
                    acceptance: vec!["Caches responses".to_string()],
                    blocked_by: vec![],
                    priority: 60,
                    state: TaskState::Review {
                        binding: None,
                        attempt: 2,
                    },
                    bounces: 1,
                    comments: vec![],
                    desk: Some("/tmp/desk/t5".into()),
                    last_report: Some("Added redis cache".to_string()),
                    last_review: Some("Need persistent backend".to_string()),
                    history: vec![],
                },
                // Task in Parked (ReviewBounceBudget) state
                Task {
                    id: TaskId("shop-crawler/e1-ingest/s1-parse/t6-db".to_string()),
                    title: "Database Integration".to_string(),
                    description: "Persist to database".to_string(),
                    acceptance: vec!["Data persists across restarts".to_string()],
                    blocked_by: vec![],
                    priority: 50,
                    state: TaskState::Parked {
                        reason: ParkReason::ReviewBounceBudget,
                        attempt: 4,
                    },
                    bounces: 4,
                    comments: vec![],
                    desk: Some("/tmp/desk/t6".into()),
                    last_report: Some("Attempted integration".to_string()),
                    last_review: Some("Schema mismatch".to_string()),
                    history: vec![],
                },
                // Task in Parked (WorkerBlocked) state
                Task {
                    id: TaskId("shop-crawler/e1-ingest/s1-parse/t7-auth".to_string()),
                    title: "Authentication".to_string(),
                    description: "Add shop auth".to_string(),
                    acceptance: vec!["Authenticates with shops".to_string()],
                    blocked_by: vec![],
                    priority: 40,
                    state: TaskState::Parked {
                        reason: ParkReason::WorkerBlocked(
                            "Need API credentials from shop".to_string(),
                        ),
                        attempt: 2,
                    },
                    bounces: 0,
                    comments: vec![],
                    desk: Some("/tmp/desk/t7".into()),
                    last_report: Some("Blocked waiting for credentials".to_string()),
                    last_review: None,
                    history: vec![],
                },
                // Task in Parked (SpawnFailed) state
                Task {
                    id: TaskId("shop-crawler/e1-ingest/s1-parse/t8-tests".to_string()),
                    title: "Unit Tests".to_string(),
                    description: "Add test coverage".to_string(),
                    acceptance: vec!["90% coverage".to_string()],
                    blocked_by: vec![],
                    priority: 30,
                    state: TaskState::Parked {
                        reason: ParkReason::SpawnFailed(
                            "Agent startup timeout".to_string(),
                        ),
                        attempt: 1,
                    },
                    bounces: 0,
                    comments: vec![],
                    desk: None,
                    last_report: None,
                    last_review: None,
                    history: vec![],
                },
                // Task in Done state
                Task {
                    id: TaskId("shop-crawler/e1-ingest/s1-parse/t9-docs".to_string()),
                    title: "Documentation".to_string(),
                    description: "Write README".to_string(),
                    acceptance: vec!["Clear setup instructions".to_string()],
                    blocked_by: vec![],
                    priority: 20,
                    state: TaskState::Done { at_ms: now + 5000 },
                    bounces: 0,
                    comments: vec![Comment {
                        id: CommentId(2),
                        author: CommentAuthor::Office,
                        text: "Great work".to_string(),
                        created_ms: now + 1000,
                        receipt: Receipt::Delivered { at_ms: now + 1200 },
                    }],
                    desk: Some("/tmp/desk/t9".into()),
                    last_report: Some("Completed documentation".to_string()),
                    last_review: Some("Approved".to_string()),
                    history: vec![TaskEvent {
                        at_ms: now + 5000,
                        event: "Completed".to_string(),
                    }],
                },
            ],
            config: ProjectConfig {
                max_workers: 2,
                bounce_budget: 3,
                worker_model: Some("claude-sonnet".to_string()),
                reviewer_model: Some("claude-opus".to_string()),
                office_role: "main".to_string(),
                worker_max_runtime_ms: 20 * 60 * 1000,
                keep_desks: true,
            },
            outbox: vec![
                OutboundNotice {
                    id: 1,
                    text: "Task 1 started".to_string(),
                    sent: true,
                    paused: false,
                },
                OutboundNotice {
                    id: 2,
                    text: "Task 6 parked".to_string(),
                    sent: false,
                    paused: true,
                },
            ],
            seq: 42,
        };

        // Serialize to JSON
        let json_str = serde_json::to_string(&project).expect("Failed to serialize");

        // Deserialize back from JSON
        let deserialized: Project =
            serde_json::from_str(&json_str).expect("Failed to deserialize");

        // Verify the roundtrip succeeded
        assert_eq!(project, deserialized);

        // Additional checks on structure
        assert_eq!(deserialized.tasks.len(), 9);
        assert_eq!(deserialized.config.max_workers, 2);
        assert_eq!(deserialized.config.bounce_budget, 3);
        assert_eq!(deserialized.seq, 42);
    }

    #[test]
    fn test_manifest_valid_json() {
        let manifest_json = include_str!("../../../manifest.json");
        let value: serde_json::Value =
            serde_json::from_str(manifest_json).expect("Manifest must be valid JSON");

        // Basic structure checks
        assert_eq!(value["schema"], "koma-extension/v0");
        assert_eq!(value["id"], "aula.workflow");
        assert_eq!(value["kind"], "daemon");

        // Check requires
        let requires = &value["requires"];
        assert!(requires.is_array());
        let requires_vec: Vec<&str> = requires
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|v| v.as_str())
            .collect();
        assert!(requires_vec.contains(&"agents:orchestrate"));
        assert!(requires_vec.contains(&"sessions:manage"));
        assert!(requires_vec.contains(&"chat:prompt"));
        assert!(requires_vec.contains(&"models:invoke"));
        assert!(requires_vec.contains(&"context:publish"));

        // Check contributes.panels
        let panels = &value["contributes"]["panels"];
        assert!(panels.is_array());
        assert!(panels[0]["id"].as_str().is_some());

        // Check contributes.sub_agents
        let sub_agents = &value["contributes"]["sub_agents"];
        assert!(sub_agents.is_array());
        assert_eq!(sub_agents.as_array().unwrap().len(), 2);
        assert_eq!(sub_agents[0]["name"], "office-worker");
        assert_eq!(sub_agents[1]["name"], "office-reviewer");

        // Check contributes.tools
        let tools = &value["contributes"]["tools"];
        assert!(tools.is_array());
        assert!(tools.as_array().unwrap().len() >= 7);
        let tool_names: Vec<&str> = tools
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|t| t["name"].as_str())
            .collect();
        assert!(tool_names.contains(&"workflow_brief"));
        assert!(tool_names.contains(&"workflow_status"));
        assert!(tool_names.contains(&"workflow_authorize"));
        assert!(tool_names.contains(&"workflow_interrupt"));
        assert!(tool_names.contains(&"workflow_resume"));
        assert!(tool_names.contains(&"workflow_comment"));
        assert!(tool_names.contains(&"workflow_projects"));
    }

    #[test]
    fn test_comment_receipt_transitions() {
        let comment_pending = Comment {
            id: CommentId(1),
            author: CommentAuthor::User,
            text: "Check this".to_string(),
            created_ms: 1000,
            receipt: Receipt::Pending,
        };

        // Can move to Delivered
        let comment_delivered = Comment {
            receipt: Receipt::Delivered { at_ms: 1100 },
            ..comment_pending.clone()
        };

        // Can move to Read from Delivered
        let comment_read = Comment {
            receipt: Receipt::Read { at_ms: 1200 },
            ..comment_delivered.clone()
        };

        assert_eq!(comment_pending.receipt, Receipt::Pending);
        assert!(matches!(comment_delivered.receipt, Receipt::Delivered { .. }));
        assert!(matches!(comment_read.receipt, Receipt::Read { .. }));
    }

    #[test]
    fn test_all_project_phases() {
        let phases = vec![
            ProjectPhase::Drafting,
            ProjectPhase::Ready,
            ProjectPhase::Running,
            ProjectPhase::Interrupted,
            ProjectPhase::Halted {
                reason: "Blocked".to_string(),
            },
            ProjectPhase::Done { at_ms: 5000 },
        ];

        // All phases should serialize and deserialize
        for phase in phases {
            let json = serde_json::to_string(&phase).expect("Failed to serialize phase");
            let deserialized: ProjectPhase =
                serde_json::from_str(&json).expect("Failed to deserialize phase");
            assert_eq!(phase, deserialized);
        }
    }

    #[test]
    fn test_agent_binding_kinds() {
        let worker_binding = AgentBinding {
            ext_agent_id: 1,
            session: "s1".to_string(),
            spawned_at_ms: 1000,
            kind: AgentKind::Worker,
        };

        let reviewer_binding = AgentBinding {
            ext_agent_id: 2,
            session: "s1".to_string(),
            spawned_at_ms: 1100,
            kind: AgentKind::Reviewer,
        };

        let json_worker = serde_json::to_string(&worker_binding).unwrap();
        let json_reviewer = serde_json::to_string(&reviewer_binding).unwrap();

        let back_worker: AgentBinding = serde_json::from_str(&json_worker).unwrap();
        let back_reviewer: AgentBinding = serde_json::from_str(&json_reviewer).unwrap();

        assert_eq!(back_worker.kind, AgentKind::Worker);
        assert_eq!(back_reviewer.kind, AgentKind::Reviewer);
    }

    #[test]
    fn test_schema_version() {
        assert_eq!(SCHEMA_V, 1);
    }

    #[test]
    fn test_project_default_config() {
        let config = ProjectConfig::default_config();
        assert_eq!(config.max_workers, 2);
        assert_eq!(config.bounce_budget, 3);
        assert_eq!(config.office_role, "main");
        assert_eq!(config.worker_max_runtime_ms, 20 * 60 * 1000);
        assert_eq!(config.worker_model, None);
        assert_eq!(config.reviewer_model, None);
    }
}
