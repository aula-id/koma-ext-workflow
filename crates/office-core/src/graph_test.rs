#[cfg(test)]
mod tests {
    use crate::domain::*;
    use crate::graph::*;

    fn task(id: &str, state: TaskState, blocked_by: &[&str], priority: i32) -> Task {
        Task {
            id: TaskId(id.to_string()),
            title: id.to_string(),
            description: String::new(),
            acceptance: vec![],
            blocked_by: blocked_by.iter().map(|b| TaskId(b.to_string())).collect(),
            priority,
            state,
            bounces: 0,
            comments: vec![],
            desk: None,
            last_report: None,
            last_review: None,
            history: vec![],
        }
    }

    fn done() -> TaskState {
        TaskState::Done { at_ms: 1 }
    }
    fn parked() -> TaskState {
        TaskState::Parked {
            reason: ParkReason::ReviewBounceBudget,
            attempt: 1,
        }
    }
    fn onprogress() -> TaskState {
        TaskState::OnProgress {
            binding: AgentBinding {
                ext_agent_id: 1,
                session: "s".to_string(),
                spawned_at_ms: 0,
                kind: AgentKind::Worker,
                persona: String::new(),
            },
            attempt: 1,
        }
    }

    fn project(tasks: Vec<Task>) -> Project {
        Project {
            id: ProjectId("p".to_string()),
            name: "p".to_string(),
            phase: ProjectPhase::Running,
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
            delivery_path: None,
            bound_session: None,
            workspace: None,
            epics: vec![],
            stories: vec![],
            tasks,
            config: ProjectConfig::default_config(),
            outbox: vec![],
            trace: vec![],
            interrupted_from: None,
            gate_cleared: false,
            pending_breakdown: None,
            seq: 0,
        }
    }

    // ---- validate_acyclic ----

    #[test]
    fn acyclic_passes() {
        let tasks = vec![
            task("a", TaskState::Todo, &[], 0),
            task("b", TaskState::Todo, &["a"], 0),
            task("c", TaskState::Todo, &["a", "b"], 0),
        ];
        assert!(validate_acyclic(&tasks).is_ok());
    }

    #[test]
    fn injected_cycle_fails() {
        let tasks = vec![
            task("a", TaskState::Todo, &["c"], 0),
            task("b", TaskState::Todo, &["a"], 0),
            task("c", TaskState::Todo, &["b"], 0),
        ];
        let err = validate_acyclic(&tasks).unwrap_err();
        assert_eq!(
            err.nodes,
            vec![
                TaskId("a".to_string()),
                TaskId("b".to_string()),
                TaskId("c".to_string())
            ]
        );
    }

    #[test]
    fn self_loop_is_a_cycle() {
        let tasks = vec![task("a", TaskState::Todo, &["a"], 0)];
        assert!(validate_acyclic(&tasks).is_err());
    }

    #[test]
    fn dangling_blocker_reference_is_ignored_for_cycles() {
        let tasks = vec![task("a", TaskState::Todo, &["ghost"], 0)];
        assert!(validate_acyclic(&tasks).is_ok());
    }

    // ---- ready_set ----

    #[test]
    fn ready_set_only_todo_with_all_deps_done() {
        let tasks = vec![
            task("a", done(), &[], 0),
            task("b", TaskState::Todo, &["a"], 0), // deps done -> ready
            task("c", TaskState::Todo, &["d"], 0), // dep not done -> not ready
            task("d", TaskState::Todo, &[], 0),    // no deps -> ready
            task("e", onprogress(), &[], 0),       // not Todo -> excluded
        ];
        let ready = ready_set(&tasks);
        assert_eq!(
            ready,
            vec![TaskId("b".to_string()), TaskId("d".to_string())]
        );
    }

    #[test]
    fn ready_set_ordering_priority_desc_then_id_asc() {
        let tasks = vec![
            task("z", TaskState::Todo, &[], 5),
            task("a", TaskState::Todo, &[], 5), // tie with z on priority -> id asc first
            task("m", TaskState::Todo, &[], 10), // highest priority first
            task("b", TaskState::Todo, &[], 1),
        ];
        let ready = ready_set(&tasks);
        assert_eq!(
            ready,
            vec![
                TaskId("m".to_string()), // prio 10
                TaskId("a".to_string()), // prio 5, id asc
                TaskId("z".to_string()), // prio 5
                TaskId("b".to_string()), // prio 1
            ]
        );
    }

    #[test]
    fn ready_set_missing_blocker_not_ready() {
        let tasks = vec![task("a", TaskState::Todo, &["ghost"], 0)];
        assert!(ready_set(&tasks).is_empty());
    }

    // ---- line_is_stuck ----

    #[test]
    fn stuck_when_all_four_conditions_hold() {
        // a parked; b,c transitively blocked by a; nothing running; nothing ready.
        let tasks = vec![
            task("a", parked(), &[], 0),
            task("b", TaskState::Todo, &["a"], 0),
            task("c", TaskState::Todo, &["b"], 0),
        ];
        let stuck = line_is_stuck(&project(tasks)).expect("should be halted");
        assert_eq!(stuck.parked_blockers, vec![TaskId("a".to_string())]);
    }

    #[test]
    fn not_stuck_when_all_done() {
        let tasks = vec![task("a", done(), &[], 0), task("b", done(), &["a"], 0)];
        assert!(line_is_stuck(&project(tasks)).is_none());
    }

    #[test]
    fn not_stuck_with_one_running_agent() {
        // a parked, but c is OnProgress -> line is still moving.
        let tasks = vec![
            task("a", parked(), &[], 0),
            task("b", TaskState::Todo, &["a"], 0),
            task("c", onprogress(), &[], 0),
        ];
        assert!(line_is_stuck(&project(tasks)).is_none());
    }

    #[test]
    fn not_stuck_with_one_ready_task() {
        // a parked, but d is a Todo with no deps -> ready, line can dispatch.
        let tasks = vec![
            task("a", parked(), &[], 0),
            task("b", TaskState::Todo, &["a"], 0),
            task("d", TaskState::Todo, &[], 0),
        ];
        assert!(line_is_stuck(&project(tasks)).is_none());
    }

    #[test]
    fn not_stuck_when_an_unfinished_task_is_not_poisoned() {
        // a parked blocks b; but e is a Backlog task not blocked by any parked task.
        // e is unfinished and NOT transitively blocked by a park, so not stuck.
        let tasks = vec![
            task("a", parked(), &[], 0),
            task("b", TaskState::Todo, &["a"], 0),
            task("e", TaskState::Backlog, &[], 0),
        ];
        assert!(line_is_stuck(&project(tasks)).is_none());
    }

    #[test]
    fn parked_task_alone_is_stuck() {
        // Only one unfinished task and it is parked -> nothing can move.
        let tasks = vec![task("a", done(), &[], 0), task("b", parked(), &[], 0)];
        let stuck = line_is_stuck(&project(tasks)).expect("should be halted");
        assert_eq!(stuck.parked_blockers, vec![TaskId("b".to_string())]);
    }
}
