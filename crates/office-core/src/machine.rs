//! Pure task/project state machines.
//!
//! `step_task` and `step_project` are total functions over the domain phase enums.
//! Every legal edge in ARCHITECTURE.md 3 is enumerated; anything else returns
//! `Err(Transition)`. The kernel (W4) never panics on user input — an illegal
//! transition is surfaced to the panel instead.

use crate::domain::{AgentBinding, ParkReason, ProjectPhase, TaskState};

/// An illegal transition. `from` is the current-state label, `attempted` is the
/// transition label. Carried to the panel so the user sees why an action bounced.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Transition {
    pub from: &'static str,
    pub attempted: &'static str,
}

impl Transition {
    fn new(from: &'static str, attempted: &'static str) -> Self {
        Transition { from, attempted }
    }
}

/// Task-level transitions. Data-carrying variants supply exactly what the target
/// `TaskState` needs; the machine never invents ids, clocks, or bindings.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TaskTransition {
    /// Backlog -> Todo (office breakdown accepted / groomed into the line).
    Groom,
    /// Todo -> OnProgress (kernel dispatch: deps Done, slot free).
    Dispatch { binding: AgentBinding, attempt: u32 },
    /// OnProgress -> Review (worker done, report parsed `status: complete`).
    /// The reviewer binding is assigned by the kernel afterwards, so review
    /// starts with `binding: None`.
    Complete,
    /// OnProgress -> Todo (worker `status: error|killed`; attempt not counted as a bounce).
    WorkerError,
    /// OnProgress -> Parked(WorkerBlocked) (report `status: blocked`).
    Block { reason: String },
    /// Review -> Done (reviewer verdict pass).
    Pass { at_ms: u64 },
    /// Review -> Todo (verdict fail, bounces within budget; notes attached by kernel).
    Bounce,
    /// Review -> Parked(ReviewBounceBudget) (bounces over budget).
    BounceOverBudget,
    /// Parked -> Todo (user un-parks / workflow_resume).
    Unpark,
    /// any non-Done -> Todo (hard interrupt normalizes in-flight states).
    HardInterrupt,
}

fn task_label(state: &TaskState) -> &'static str {
    match state {
        TaskState::Backlog => "Backlog",
        TaskState::Todo => "Todo",
        TaskState::OnProgress { .. } => "OnProgress",
        TaskState::Review { .. } => "Review",
        TaskState::Parked { .. } => "Parked",
        TaskState::Done { .. } => "Done",
    }
}

fn task_transition_label(t: &TaskTransition) -> &'static str {
    match t {
        TaskTransition::Groom => "Groom",
        TaskTransition::Dispatch { .. } => "Dispatch",
        TaskTransition::Complete => "Complete",
        TaskTransition::WorkerError => "WorkerError",
        TaskTransition::Block { .. } => "Block",
        TaskTransition::Pass { .. } => "Pass",
        TaskTransition::Bounce => "Bounce",
        TaskTransition::BounceOverBudget => "BounceOverBudget",
        TaskTransition::Unpark => "Unpark",
        TaskTransition::HardInterrupt => "HardInterrupt",
    }
}

/// Advance a single task's state. Pure and deterministic.
pub fn step_task(state: &TaskState, t: TaskTransition) -> Result<TaskState, Transition> {
    let err = || Transition::new(task_label(state), task_transition_label(&t));

    // Hard interrupt normalizes any in-flight/non-terminal state to Todo. Done is
    // terminal and cannot be interrupted.
    if let TaskTransition::HardInterrupt = t {
        return match state {
            TaskState::Done { .. } => Err(err()),
            _ => Ok(TaskState::Todo),
        };
    }

    match (state, &t) {
        (TaskState::Backlog, TaskTransition::Groom) => Ok(TaskState::Todo),

        (TaskState::Todo, TaskTransition::Dispatch { binding, attempt }) => {
            Ok(TaskState::OnProgress {
                binding: binding.clone(),
                attempt: *attempt,
            })
        }

        (TaskState::OnProgress { attempt, .. }, TaskTransition::Complete) => {
            Ok(TaskState::Review {
                binding: None,
                attempt: *attempt,
            })
        }
        (TaskState::OnProgress { .. }, TaskTransition::WorkerError) => Ok(TaskState::Todo),
        (TaskState::OnProgress { attempt, .. }, TaskTransition::Block { reason }) => {
            Ok(TaskState::Parked {
                reason: ParkReason::WorkerBlocked(reason.clone()),
                attempt: *attempt,
            })
        }

        (TaskState::Review { .. }, TaskTransition::Pass { at_ms }) => {
            Ok(TaskState::Done { at_ms: *at_ms })
        }
        (TaskState::Review { .. }, TaskTransition::Bounce) => Ok(TaskState::Todo),
        (TaskState::Review { attempt, .. }, TaskTransition::BounceOverBudget) => {
            Ok(TaskState::Parked {
                reason: ParkReason::ReviewBounceBudget,
                attempt: *attempt,
            })
        }

        (TaskState::Parked { .. }, TaskTransition::Unpark) => Ok(TaskState::Todo),

        _ => Err(err()),
    }
}

/// Project-level transitions.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ProjectTransition {
    /// Drafting -> Ready (office breakdown accepted by user).
    AcceptBreakdown,
    /// Ready -> Running. Gated: `delivery_path_valid` must be true (the hard
    /// authorization gate, ARCHITECTURE.md 6.3.3). A false flag is an illegal edge.
    Authorize { delivery_path_valid: bool },
    /// Running -> Interrupted AND Drafting -> Interrupted (interrupt button /
    /// workflow_interrupt). Drafting is included so any dangling drafting process
    /// (research/audit analyst, in-flight persona/assume-check invoke) can be cut off from
    /// the very start of PRD drafting (feature: interrupt-from-drafting).
    Interrupt,
    /// Interrupted -> (Running | Drafting) and Halted -> Running (resume). `to_drafting` is
    /// supplied by the kernel from `Project.interrupted_from`: a Drafting-interrupt resumes
    /// back to Drafting, every other interrupt (and a Halt) resumes to Running. Carried on the
    /// transition — like `Authorize`'s `delivery_path_valid` — so the pure machine stays the
    /// single source of truth for the resume target.
    Resume { to_drafting: bool },
    /// Running -> Halted (kernel: line stuck).
    Halt { reason: String },
    /// Running -> Done (all tasks Done).
    Complete { at_ms: u64 },
    /// Ready -> Drafting AND Drafting -> Drafting (SDLC re-triage / escalation to a heavier track,
    /// pre-authorize only — feature: sdlc-triage). A light track (patch/enhancement) whose board is
    /// already built in Ready can be sent back to Drafting to re-run the fuller ceremony; the
    /// Drafting -> Drafting self-edge lets the kernel call this uniformly without special-casing the
    /// phase. Never legal from Running/Interrupted/Halted/Done — escalation is pre-authorize.
    Retriage,
}

fn phase_label(phase: &ProjectPhase) -> &'static str {
    match phase {
        ProjectPhase::Drafting => "Drafting",
        ProjectPhase::Ready => "Ready",
        ProjectPhase::Running => "Running",
        ProjectPhase::Interrupted => "Interrupted",
        ProjectPhase::Halted { .. } => "Halted",
        ProjectPhase::Done { .. } => "Done",
    }
}

fn project_transition_label(t: &ProjectTransition) -> &'static str {
    match t {
        ProjectTransition::AcceptBreakdown => "AcceptBreakdown",
        ProjectTransition::Authorize { .. } => "Authorize",
        ProjectTransition::Interrupt => "Interrupt",
        ProjectTransition::Resume { .. } => "Resume",
        ProjectTransition::Halt { .. } => "Halt",
        ProjectTransition::Complete { .. } => "Complete",
        ProjectTransition::Retriage => "Retriage",
    }
}

/// Advance a project's phase. Pure and deterministic.
pub fn step_project(
    phase: &ProjectPhase,
    t: ProjectTransition,
) -> Result<ProjectPhase, Transition> {
    let err = || Transition::new(phase_label(phase), project_transition_label(&t));

    match (phase, &t) {
        (ProjectPhase::Drafting, ProjectTransition::AcceptBreakdown) => Ok(ProjectPhase::Ready),

        (ProjectPhase::Ready, ProjectTransition::Authorize { delivery_path_valid }) => {
            if *delivery_path_valid {
                Ok(ProjectPhase::Running)
            } else {
                // The delivery-path gate: no valid path -> the project CANNOT leave Ready.
                Err(err())
            }
        }

        (ProjectPhase::Running, ProjectTransition::Interrupt) => Ok(ProjectPhase::Interrupted),
        // Drafting is interruptible too (feature: interrupt-from-drafting) so a dangling
        // drafting process can be cut off before the board even exists.
        (ProjectPhase::Drafting, ProjectTransition::Interrupt) => Ok(ProjectPhase::Interrupted),
        (ProjectPhase::Running, ProjectTransition::Halt { reason }) => Ok(ProjectPhase::Halted {
            reason: reason.clone(),
        }),
        (ProjectPhase::Running, ProjectTransition::Complete { at_ms }) => {
            Ok(ProjectPhase::Done { at_ms: *at_ms })
        }

        (ProjectPhase::Interrupted, ProjectTransition::Resume { to_drafting }) => {
            if *to_drafting {
                Ok(ProjectPhase::Drafting)
            } else {
                Ok(ProjectPhase::Running)
            }
        }
        (ProjectPhase::Halted { .. }, ProjectTransition::Resume { .. }) => Ok(ProjectPhase::Running),

        // SDLC re-triage / escalation to a heavier track (pre-authorize only): Ready -> Drafting to
        // rebuild the board under the fuller ceremony, or a Drafting -> Drafting self-edge so the
        // kernel need not branch on the current phase.
        (ProjectPhase::Ready, ProjectTransition::Retriage) => Ok(ProjectPhase::Drafting),
        (ProjectPhase::Drafting, ProjectTransition::Retriage) => Ok(ProjectPhase::Drafting),

        _ => Err(err()),
    }
}
