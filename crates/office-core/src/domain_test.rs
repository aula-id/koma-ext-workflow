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
                    persona: String::new(),
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
            trd_markdown: "# TRD\n| Layer | Choice |\n|---|---|\n| Language | Rust 1.80 |".to_string(),
            research_notes: "- reqwest 0.12 for HTTP\n- scraper 0.20 for HTML parsing".to_string(),
            research: Some(AgentBinding {
                ext_agent_id: 88,
                session: "session-123".to_string(),
                spawned_at_ms: now + 50,
                kind: AgentKind::Researcher,
                persona: String::new(),
            }),
            crd_markdown: "# CRD\n- README present (10 pts)\n- builds clean (20 pts)".to_string(),
            audit: Some(AgentBinding {
                ext_agent_id: 91,
                session: "session-123".to_string(),
                spawned_at_ms: now + 900,
                kind: AgentKind::Auditor,
                persona: String::new(),
            }),
            audit_rounds: 1,
            last_audit_grade: Some(93),
            pending_assumptions: vec!["assumed Postgres, user never stated a DB".to_string()],
            assumptions_approved: true,
            self_resolved_assumptions: vec!["assumed nightly cron, self-resolved under approval".to_string()],
            capture_nudge_count: 2,
            assumption_rounds: 1,
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
                            persona: "office-worker-nova".to_string(),
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
                            persona: "office-reviewer".to_string(),
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
                crd_pass_grade: 95,
                assumption_check: false,
                safeguard_role: "safeguard".to_string(),
                assumption_mode: "ask".to_string(),
                research_mode: "always".to_string(),
                drafter_model: Some("claude-opus".to_string()),
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
            trace: vec![TraceEvent {
                ts: now as i64,
                kind: "phase".to_string(),
                summary: "hard interrupt from running".to_string(),
            }],
            interrupted_from: Some(ProjectPhase::Running),
            gate_cleared: false,
            gate_invoke_live_hint: false,
            pending_breakdown: None,
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
        // The 6.2b additive fields round-trip too.
        assert!(deserialized.trd_markdown.contains("Rust 1.80"));
        assert!(deserialized.research_notes.contains("reqwest 0.12"));
        assert_eq!(deserialized.research.as_ref().unwrap().kind, AgentKind::Researcher);
        // The 6.2c additive fields (CRD + audit + safeguard) round-trip too.
        assert!(deserialized.crd_markdown.contains("README present"));
        assert_eq!(deserialized.audit.as_ref().unwrap().kind, AgentKind::Auditor);
        assert_eq!(deserialized.audit_rounds, 1);
        assert_eq!(deserialized.last_audit_grade, Some(93));
        assert_eq!(deserialized.pending_assumptions.len(), 1);
        assert_eq!(deserialized.config.crd_pass_grade, 95);
        assert!(!deserialized.config.assumption_check);
        assert_eq!(deserialized.config.safeguard_role, "safeguard");
        // The approval/nudge additive fields + the autonomous-safeguard fields all round-trip.
        assert!(deserialized.assumptions_approved, "assumptions_approved round-trips");
        assert_eq!(deserialized.self_resolved_assumptions.len(), 1);
        assert_eq!(deserialized.capture_nudge_count, 2);
        assert_eq!(deserialized.assumption_rounds, 1);
        assert_eq!(deserialized.config.assumption_mode, "ask");
    }

    #[test]
    fn test_project_loads_pre_6_2b_json_without_the_new_fields() {
        // A state.json written before the PRD->research->TRD pipeline existed carries no
        // `trd_markdown`/`research_notes`/`research`. `#[serde(default)]` must let it load
        // clean, defaulting the three fields to empty/None (schema stays workflow/1).
        let legacy = r##"{
            "id": "legacy",
            "name": "Legacy",
            "phase": "Drafting",
            "prd_markdown": "# PRD\nold",
            "office_transcript": [],
            "office_summary": "",
            "delivery_path": null,
            "bound_session": null,
            "workspace": null,
            "epics": [],
            "stories": [],
            "tasks": [],
            "config": {
                "max_workers": 2,
                "bounce_budget": 3,
                "worker_model": null,
                "reviewer_model": null,
                "office_role": "main",
                "worker_max_runtime_ms": 1200000
            },
            "outbox": [],
            "seq": 0
        }"##;

        let p: Project = serde_json::from_str(legacy).expect("legacy JSON must load clean");
        assert_eq!(p.prd_markdown, "# PRD\nold");
        assert_eq!(p.trd_markdown, "", "absent trd_markdown defaults to empty");
        assert_eq!(p.research_notes, "", "absent research_notes defaults to empty");
        assert!(p.research.is_none(), "absent research binding defaults to None");
        // 6.2c additive fields also default cleanly on a pre-6.2c state file: the CRD is empty,
        // no audit ran, and the config's named-fn defaults (not 0/false) take hold so the gate
        // is not silently disabled on legacy projects.
        assert_eq!(p.crd_markdown, "", "absent crd_markdown defaults to empty");
        assert!(p.audit.is_none() && p.audit_rounds == 0 && p.last_audit_grade.is_none());
        assert!(p.pending_assumptions.is_empty());
        assert_eq!(p.config.crd_pass_grade, 98, "absent crd_pass_grade defaults to 98, not 0");
        assert!(p.config.assumption_check, "absent assumption_check defaults to true, not false");
        assert_eq!(p.config.safeguard_role, "safeguard");
        // The approval/nudge additive fields default cleanly on a legacy state file. Autonomous-
        // safeguard defaults: a legacy project loads FULLY AUTONOMOUS ("auto"), round 0.
        assert!(!p.assumptions_approved, "absent assumptions_approved defaults to false");
        assert!(p.self_resolved_assumptions.is_empty(), "absent self_resolved_assumptions defaults to empty");
        assert_eq!(p.capture_nudge_count, 0, "absent capture_nudge_count defaults to 0");
        assert_eq!(p.assumption_rounds, 0, "absent assumption_rounds defaults to 0");
        assert_eq!(
            p.config.assumption_mode, "auto",
            "absent assumption_mode defaults to 'auto' (autonomous), not empty"
        );
        // Design-speedup additive fields also default cleanly on a pre-6.2b-design-speedup state
        // file: the one-shot gate has not (yet, on THIS load) been recorded as cleared, no early
        // breakdown is stashed, and research runs in the default "auto" mode with no drafter model
        // override (review finding: these defaults previously went unasserted here).
        assert!(!p.gate_cleared, "absent gate_cleared defaults to false");
        assert!(p.pending_breakdown.is_none(), "absent pending_breakdown defaults to None");
        assert_eq!(p.config.research_mode, "auto", "absent research_mode defaults to 'auto'");
        assert!(p.config.drafter_model.is_none(), "absent drafter_model defaults to None");
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

        // Check contributes.sub_agents: a POOL of 10 worker personas (office-worker-<name>)
        // in a stable order, then the three unchanged fixed staff.
        let sub_agents = &value["contributes"]["sub_agents"];
        assert!(sub_agents.is_array());
        let sub = sub_agents.as_array().unwrap();
        assert_eq!(sub.len(), 13);

        let pool = ["nova", "mika", "tetsuo", "bob", "yuki", "dax", "ines", "koji", "vera", "pip"];
        for (i, name) in pool.iter().enumerate() {
            assert_eq!(sub[i]["name"], format!("office-worker-{name}"));
        }
        assert_eq!(sub[10]["name"], "office-reviewer");
        assert_eq!(sub[11]["name"], "office-researcher");
        assert_eq!(sub[12]["name"], "office-auditor");
        // The pool order MUST match office-core's WORKER_PERSONAS (persona assignment relies on it).
        assert_eq!(pool, crate::persona::WORKER_PERSONAS);

        // Every worker persona shares the SAME builder tools allow-list (write/edit/bash) and the
        // SAME byte-identical protocol CORE; only a trailing personality flavor differs, and each
        // carries the guard line so personality never overrides the protocol.
        const GUARD_LINE: &str =
            "Your personality colors your notes and summaries only — never the work protocol.";
        // The shared CORE is everything before the LAST blank line (the flavor paragraph).
        let core_of = |prompt: &str| -> String {
            prompt.rsplit_once("\n\n").map(|(head, _)| head.to_string()).unwrap_or_default()
        };
        let base_tools = sub[0]["tools"].as_array().unwrap();
        assert!(base_tools.iter().any(|t| t == "write"));
        assert!(base_tools.iter().any(|t| t == "edit"));
        assert!(base_tools.iter().any(|t| t == "bash"));
        let base_core = core_of(sub[0]["prompt"].as_str().unwrap());
        assert!(base_core.starts_with("You are a Workflow worker on one task"));
        for i in 0..pool.len() {
            let prompt = sub[i]["prompt"].as_str().unwrap();
            assert_eq!(sub[i]["tools"].as_array().unwrap(), base_tools, "identical worker tools");
            assert_eq!(core_of(prompt), base_core, "byte-identical protocol core across personas");
            assert!(prompt.contains(GUARD_LINE), "persona {i} carries the guard line");
            assert!(prompt.contains("mcp__workflow__*"), "persona {i} keeps the MCP loop guard");
        }

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
            persona: "office-worker-nova".to_string(),
        };

        let reviewer_binding = AgentBinding {
            ext_agent_id: 2,
            session: "s1".to_string(),
            spawned_at_ms: 1100,
            kind: AgentKind::Reviewer,
            persona: "office-reviewer".to_string(),
        };

        let json_worker = serde_json::to_string(&worker_binding).unwrap();
        let json_reviewer = serde_json::to_string(&reviewer_binding).unwrap();

        let back_worker: AgentBinding = serde_json::from_str(&json_worker).unwrap();
        let back_reviewer: AgentBinding = serde_json::from_str(&json_reviewer).unwrap();

        assert_eq!(back_worker.kind, AgentKind::Worker);
        assert_eq!(back_reviewer.kind, AgentKind::Reviewer);
        // The persona label round-trips verbatim.
        assert_eq!(back_worker.persona, "office-worker-nova");
        assert_eq!(back_reviewer.persona, "office-reviewer");
    }

    #[test]
    fn test_agent_binding_persona_serde_default() {
        // A binding persisted before personas existed carries no `persona`; `#[serde(default)]`
        // must load it clean as an empty string (old state loads without a migration).
        let legacy = r#"{ "ext_agent_id": 5, "session": "s", "spawned_at_ms": 1, "kind": "Worker" }"#;
        let b: AgentBinding = serde_json::from_str(legacy).expect("legacy binding must load clean");
        assert_eq!(b.persona, "", "absent persona defaults to empty");
        assert_eq!(b.kind, AgentKind::Worker);

        // A fresh worker binding carries its `office-worker-<name>` persona and round-trips.
        let fresh = AgentBinding {
            ext_agent_id: 9,
            session: "s".to_string(),
            spawned_at_ms: 2,
            kind: AgentKind::Worker,
            persona: worker_agent_id("some/task/id"),
        };
        assert!(fresh.persona.starts_with("office-worker-"));
        let back: AgentBinding =
            serde_json::from_str(&serde_json::to_string(&fresh).unwrap()).unwrap();
        assert_eq!(back, fresh);
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
        // 6.2c defaults: the safeguard gate is ON, the clean-build pass grade is a strict 98,
        // and the safeguard resolves against the "safeguard" role.
        assert_eq!(config.crd_pass_grade, 98);
        assert!(config.assumption_check);
        assert_eq!(config.safeguard_role, "safeguard");
        // Assumption mode defaults to "auto" (ULTRA-AUTOMATIC / autonomous, the post-unification
        // default); only ConfigSet flips it to "ask" (freeze-and-ask).
        assert_eq!(config.assumption_mode, "auto", "assumption mode defaults to auto (autonomous)");
    }
}
