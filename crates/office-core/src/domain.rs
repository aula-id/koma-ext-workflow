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
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct Project {
    pub id: ProjectId,
    pub name: String,
    pub phase: ProjectPhase,
    pub prd_markdown: String,
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
