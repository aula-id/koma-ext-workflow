use serde::{Deserialize, Serialize};
use std::path::PathBuf;

pub const SCHEMA_V: u32 = 1;

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct ProjectId(pub String);

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct EpicId(pub String);

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct StoryId(pub String);

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct TaskId(pub String);

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
pub struct CommentId(pub u64);

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct AgentBinding {
    pub ext_agent_id: u64,
    pub session: String,
    pub spawned_at_ms: u64,
    pub kind: AgentKind,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub enum AgentKind {
    Worker,
    Reviewer,
    /// The web-research analyst spawned once per project during Drafting (PRD -> research
    /// -> TRD -> breakdown, ARCHITECTURE.md 6.2b). Project-level, not task-level.
    Researcher,
    /// The read-only clean-build auditor spawned when the last task would complete a project
    /// that carries a CRD (ARCHITECTURE.md 6.2c). Grades the delivery against the CRD checklist
    /// and files an `OFFICE-AUDIT` block. Project-level, like the researcher — never a task
    /// binding.
    Auditor,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
pub enum Column {
    Backlog,
    Todo,
    OnProgress,
    Review,
    Done,
}

pub fn column(state: &TaskState) -> Column {
    match state {
        TaskState::Backlog => Column::Backlog,
        TaskState::Todo => Column::Todo,
        TaskState::OnProgress { .. } => Column::OnProgress,
        TaskState::Review { .. } => Column::Review,
        TaskState::Parked { .. } => Column::Review,
        TaskState::Done { .. } => Column::Done,
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub enum TaskState {
    Backlog,
    Todo,
    OnProgress {
        binding: AgentBinding,
        attempt: u32,
    },
    Review {
        binding: Option<AgentBinding>,
        attempt: u32,
    },
    Parked {
        reason: ParkReason,
        attempt: u32,
    },
    Done {
        at_ms: u64,
    },
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub enum ParkReason {
    ReviewBounceBudget,
    WorkerBlocked(String),
    SpawnFailed(String),
    /// The CRD clean-build audit kept grading the delivery below `crd_pass_grade` across the
    /// two automated remediation rounds (ARCHITECTURE.md 6.2c). Carries the last grade + top
    /// failures so the halt/attention lines can explain it. The user fixes manually and
    /// unparks, or lowers the threshold in settings.
    AuditFailed(String),
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct Task {
    pub id: TaskId,
    pub title: String,
    pub description: String,
    pub acceptance: Vec<String>,
    pub blocked_by: Vec<TaskId>,
    pub priority: i32,
    pub state: TaskState,
    pub bounces: u32,
    pub comments: Vec<Comment>,
    pub desk: Option<PathBuf>,
    pub last_report: Option<String>,
    pub last_review: Option<String>,
    pub history: Vec<TaskEvent>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct Comment {
    pub id: CommentId,
    pub author: CommentAuthor,
    pub text: String,
    pub created_ms: u64,
    pub receipt: Receipt,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub enum CommentAuthor {
    User,
    Office,
    System,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub enum Receipt {
    Pending,
    Delivered { at_ms: u64 },
    Read { at_ms: u64 },
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct TaskEvent {
    pub at_ms: u64,
    pub event: String,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct Story {
    pub id: StoryId,
    pub title: String,
    pub intent: String,
    pub tasks: Vec<TaskId>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct Epic {
    pub id: EpicId,
    pub title: String,
    pub intent: String,
    pub stories: Vec<StoryId>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub enum ProjectPhase {
    Drafting,
    Ready,
    Running,
    Interrupted,
    Halted { reason: String },
    Done { at_ms: u64 },
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct ChatMsg {
    pub who: ChatAuthor,
    pub text: String,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub enum ChatAuthor {
    User,
    Office,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct OutboundNotice {
    pub id: u64,
    pub text: String,
    pub sent: bool,
    pub paused: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct ProjectConfig {
    pub max_workers: u32,
    pub bounce_budget: u32,
    pub worker_model: Option<String>,
    pub reviewer_model: Option<String>,
    pub office_role: String,
    pub worker_max_runtime_ms: u64,
    /// Retain a task's desk directory (`desks/<project>/<task>--koma-workflow-desk/`,
    /// kernel.rs `desk_dir`) after the task completes, instead of it being reclaimed.
    /// `#[serde(default)]` so state files persisted before this field existed still
    /// deserialize cleanly (defaults to `false`, matching the panel's initial toggle).
    #[serde(default)]
    pub keep_desks: bool,
    /// Minimum CRD audit grade (0-100) required to complete a project instead of opening a
    /// remediation round (6.2c). A named-fn serde default (not `#[serde(default)]`, which would
    /// force `0`) so pre-6.2c state files load with the intended `98`, not a wide-open 0.
    #[serde(default = "default_crd_pass_grade")]
    pub crd_pass_grade: u32,
    /// Whether the no-assume safeguard gate runs after each drafting doc capture (6.2c). Named-fn
    /// default `true` (not `#[serde(default)]`, which would force `false` and silently disable the
    /// gate on every legacy state file).
    #[serde(default = "default_assumption_check")]
    pub assumption_check: bool,
    /// The koma role the safeguard assumption-check invoke resolves against, host-side (6.2c) —
    /// like `office_role` for the persona. Named-fn default `"safeguard"`; no panel affordance.
    #[serde(default = "default_safeguard_role")]
    pub safeguard_role: String,
}

fn default_crd_pass_grade() -> u32 {
    98
}

fn default_assumption_check() -> bool {
    true
}

fn default_safeguard_role() -> String {
    "safeguard".to_string()
}

impl ProjectConfig {
    pub fn default_config() -> Self {
        Self {
            max_workers: 2,
            bounce_budget: 3,
            worker_model: None,
            reviewer_model: None,
            office_role: "main".to_string(),
            worker_max_runtime_ms: 20 * 60 * 1000, // 20 minutes
            keep_desks: false,
            crd_pass_grade: default_crd_pass_grade(),
            assumption_check: default_assumption_check(),
            safeguard_role: default_safeguard_role(),
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct Project {
    pub id: ProjectId,
    pub name: String,
    pub phase: ProjectPhase,
    pub prd_markdown: String,
    /// The Technical Requirements Document, authored after web-research in the Drafting
    /// pipeline (PRD -> research -> TRD -> breakdown, ARCHITECTURE.md 6.2b). `#[serde(default)]`
    /// so state files persisted before this field existed still deserialize (empty by default).
    #[serde(default)]
    pub trd_markdown: String,
    /// Web-research findings gathered by the `office-researcher` sub-agent before the TRD is
    /// drafted. Stored capped at 16KB (the writer truncates with a marker). `#[serde(default)]`
    /// for the same back-compat reason as `trd_markdown`.
    #[serde(default)]
    pub research_notes: String,
    /// The project-level research sub-agent binding, present only while the office-researcher
    /// is in flight during Drafting. Two-phase like a worker binding (provisional id 0 until
    /// the driver reports the real one). `#[serde(default)]` for back-compat.
    #[serde(default)]
    pub research: Option<AgentBinding>,
    /// The Clean-build Requirement Document (CRD): a concrete gradeable checklist for THIS
    /// project (expected file-tree shape, no unwired/trash files, build+lint pass, README, ...)
    /// plus a 100-point grading rubric, authored after the TRD in Drafting (ARCHITECTURE.md
    /// 6.2c). It is the checklist the completion-time auditor grades against. `#[serde(default)]`
    /// so pre-6.2c state files deserialize (empty by default).
    #[serde(default)]
    pub crd_markdown: String,
    /// The project-level clean-build auditor binding, present only while the office-auditor is
    /// in flight at project completion (6.2c). Two-phase + reconcile-covered exactly like
    /// `research`. `#[serde(default)]` for back-compat.
    #[serde(default)]
    pub audit: Option<AgentBinding>,
    /// How many CRD audits have completed with a sub-`crd_pass_grade` grade (6.2c). Tracked
    /// additively so the remediation ladder (2 automated Todo rounds, then a parked task) is
    /// reconstructible after a store reload. `#[serde(default)]` for back-compat.
    #[serde(default)]
    pub audit_rounds: u32,
    /// The most recent CRD audit grade (0-100), surfaced on the snapshot / dashboard row / MCP
    /// status line when present (6.2c). `#[serde(default)]` (None) for back-compat.
    #[serde(default)]
    pub last_audit_grade: Option<u32>,
    /// Ungrounded assumptions the safeguard flagged in the LAST doc gate (6.2c/safeguard). Non-
    /// empty means the drafting pipeline is stopped waiting on the user to approve/answer/delegate;
    /// a subsequent clean check on any doc clears it. `#[serde(default)]` for back-compat.
    #[serde(default)]
    pub pending_assumptions: Vec<String>,
    pub office_transcript: Vec<ChatMsg>,
    pub office_summary: String,
    pub delivery_path: Option<PathBuf>,
    pub bound_session: Option<String>,
    pub workspace: Option<PathBuf>,
    pub epics: Vec<Epic>,
    pub stories: Vec<Story>,
    pub tasks: Vec<Task>,
    pub config: ProjectConfig,
    pub outbox: Vec<OutboundNotice>,
    pub seq: u64,
}

#[derive(Clone, Copy, Debug)]
pub enum IdMintError {
    InvalidSlug,
}

pub fn mint_id(slug: &str) -> Result<String, IdMintError> {
    if slug.is_empty() {
        return Err(IdMintError::InvalidSlug);
    }
    if !slug.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-') {
        return Err(IdMintError::InvalidSlug);
    }
    Ok(slug.to_string())
}

pub fn mint_id_with_suffix(slug: &str, suffix: u64) -> Result<String, IdMintError> {
    let base_id = mint_id(slug)?;
    Ok(format!("{}-{}", base_id, suffix))
}
