#[cfg(test)]
mod tests {
    use crate::domain::*;
    use crate::machine::*;

    fn worker_binding() -> AgentBinding {
        AgentBinding {
            ext_agent_id: 7,
            session: "s1".to_string(),
            spawned_at_ms: 1000,
            kind: AgentKind::Worker,
        }
    }

    fn onprogress() -> TaskState {
        TaskState::OnProgress {
            binding: worker_binding(),
            attempt: 2,
        }
    }

    fn review(binding: bool) -> TaskState {
        TaskState::Review {
            binding: if binding {
                Some(AgentBinding {
                    kind: AgentKind::Reviewer,
                    ..worker_binding()
                })
            } else {
                None
            },
            attempt: 2,
        }
    }

    fn parked() -> TaskState {
        TaskState::Parked {
            reason: ParkReason::ReviewBounceBudget,
            attempt: 3,
        }
    }

    // ---- Task machine: legal transitions ----

    #[test]
    fn task_legal_edges() {
        // Backlog -> Todo
        assert_eq!(
            step_task(&TaskState::Backlog, TaskTransition::Groom).unwrap(),
            TaskState::Todo
        );

        // Todo -> OnProgress carries binding + attempt
        let got = step_task(
            &TaskState::Todo,
            TaskTransition::Dispatch {
                binding: worker_binding(),
                attempt: 1,
            },
        )
        .unwrap();
        assert_eq!(
            got,
            TaskState::OnProgress {
                binding: worker_binding(),
                attempt: 1
            }
        );

        // OnProgress -> Review (binding None, attempt preserved)
        assert_eq!(
            step_task(&onprogress(), TaskTransition::Complete).unwrap(),
            TaskState::Review {
                binding: None,
                attempt: 2
            }
        );

        // OnProgress -> Todo (worker error)
        assert_eq!(
            step_task(&onprogress(), TaskTransition::WorkerError).unwrap(),
            TaskState::Todo
        );

        // OnProgress -> Parked(WorkerBlocked)
        assert_eq!(
            step_task(
                &onprogress(),
                TaskTransition::Block {
                    reason: "need creds".to_string()
                }
            )
            .unwrap(),
            TaskState::Parked {
                reason: ParkReason::WorkerBlocked("need creds".to_string()),
                attempt: 2
            }
        );

        // Review -> Done
        assert_eq!(
            step_task(&review(true), TaskTransition::Pass { at_ms: 9000 }).unwrap(),
            TaskState::Done { at_ms: 9000 }
        );

        // Review -> Todo (bounce within budget)
        assert_eq!(
            step_task(&review(true), TaskTransition::Bounce).unwrap(),
            TaskState::Todo
        );

        // Review -> Parked(ReviewBounceBudget)
        assert_eq!(
            step_task(&review(false), TaskTransition::BounceOverBudget).unwrap(),
            TaskState::Parked {
                reason: ParkReason::ReviewBounceBudget,
                attempt: 2
            }
        );

        // Parked -> Todo (unpark)
        assert_eq!(
            step_task(&parked(), TaskTransition::Unpark).unwrap(),
            TaskState::Todo
        );
    }

    #[test]
    fn hard_interrupt_normalizes_all_non_done() {
        for state in [
            TaskState::Backlog,
            TaskState::Todo,
            onprogress(),
            review(true),
            review(false),
            parked(),
        ] {
            assert_eq!(
                step_task(&state, TaskTransition::HardInterrupt).unwrap(),
                TaskState::Todo,
                "state {:?} should normalize to Todo",
                state
            );
        }
    }

    #[test]
    fn hard_interrupt_rejects_done() {
        assert!(step_task(&TaskState::Done { at_ms: 1 }, TaskTransition::HardInterrupt).is_err());
    }

    // ---- Task machine: a sample of illegal edges ----

    #[test]
    fn task_illegal_edges() {
        // cannot dispatch a Backlog task
        assert!(step_task(
            &TaskState::Backlog,
            TaskTransition::Dispatch {
                binding: worker_binding(),
                attempt: 1
            }
        )
        .is_err());
        // cannot groom a Todo task
        assert!(step_task(&TaskState::Todo, TaskTransition::Groom).is_err());
        // cannot pass a task that is not in Review
        assert!(step_task(&onprogress(), TaskTransition::Pass { at_ms: 1 }).is_err());
        // cannot complete a Todo
        assert!(step_task(&TaskState::Todo, TaskTransition::Complete).is_err());
        // cannot unpark something not parked
        assert!(step_task(&review(true), TaskTransition::Unpark).is_err());
        // Done is terminal for every transition
        assert!(step_task(&TaskState::Done { at_ms: 1 }, TaskTransition::Groom).is_err());
        assert!(step_task(&TaskState::Done { at_ms: 1 }, TaskTransition::Bounce).is_err());
    }

    #[test]
    fn task_error_carries_labels() {
        let e = step_task(&TaskState::Todo, TaskTransition::Groom).unwrap_err();
        assert_eq!(e.from, "Todo");
        assert_eq!(e.attempted, "Groom");
    }

    // ---- Project machine ----

    #[test]
    fn project_legal_edges() {
        assert_eq!(
            step_project(&ProjectPhase::Drafting, ProjectTransition::AcceptBreakdown).unwrap(),
            ProjectPhase::Ready
        );
        assert_eq!(
            step_project(
                &ProjectPhase::Ready,
                ProjectTransition::Authorize {
                    delivery_path_valid: true
                }
            )
            .unwrap(),
            ProjectPhase::Running
        );
        assert_eq!(
            step_project(&ProjectPhase::Running, ProjectTransition::Interrupt).unwrap(),
            ProjectPhase::Interrupted
        );
        assert_eq!(
            step_project(&ProjectPhase::Interrupted, ProjectTransition::Resume).unwrap(),
            ProjectPhase::Running
        );
        assert_eq!(
            step_project(
                &ProjectPhase::Running,
                ProjectTransition::Halt {
                    reason: "t1 blocks all".to_string()
                }
            )
            .unwrap(),
            ProjectPhase::Halted {
                reason: "t1 blocks all".to_string()
            }
        );
        assert_eq!(
            step_project(
                &ProjectPhase::Halted {
                    reason: "x".to_string()
                },
                ProjectTransition::Resume
            )
            .unwrap(),
            ProjectPhase::Running
        );
        assert_eq!(
            step_project(&ProjectPhase::Running, ProjectTransition::Complete { at_ms: 42 }).unwrap(),
            ProjectPhase::Done { at_ms: 42 }
        );
    }

    #[test]
    fn delivery_path_gate_blocks_ready_to_running() {
        let e = step_project(
            &ProjectPhase::Ready,
            ProjectTransition::Authorize {
                delivery_path_valid: false,
            },
        )
        .unwrap_err();
        assert_eq!(e.from, "Ready");
        assert_eq!(e.attempted, "Authorize");
    }

    #[test]
    fn project_illegal_edges() {
        // cannot authorize from Drafting (must accept breakdown first)
        assert!(step_project(
            &ProjectPhase::Drafting,
            ProjectTransition::Authorize {
                delivery_path_valid: true
            }
        )
        .is_err());
        // cannot interrupt a Ready project
        assert!(step_project(&ProjectPhase::Ready, ProjectTransition::Interrupt).is_err());
        // cannot resume a Running project
        assert!(step_project(&ProjectPhase::Running, ProjectTransition::Resume).is_err());
        // cannot complete an Interrupted project
        assert!(
            step_project(&ProjectPhase::Interrupted, ProjectTransition::Complete { at_ms: 1 })
                .is_err()
        );
        // Done is terminal
        assert!(step_project(
            &ProjectPhase::Done { at_ms: 1 },
            ProjectTransition::Resume
        )
        .is_err());
    }
}
