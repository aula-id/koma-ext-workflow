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
    /// The sub-agent persona this binding was spawned as: a worker persona id
    /// (`office-worker-<name>`, one of 10 — see `persona.rs`) on a worker dispatch, or
    /// `office-reviewer` on a reviewer dispatch. `#[serde(default)]` (empty string) so
    /// state persisted before personas existed still deserializes cleanly. The office
    /// view (ARCHITECTURE.md 5.2 / digest.rs) strips the `office-worker-` prefix to the
    /// short name; the value never overrides the state machine, it only labels the desk.
    #[serde(default)]
    pub persona: String,
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

/// One machine-diary entry (feature: tracelog) — a single line of what the office machine DID,
/// not what a document says. `ts` is epoch milliseconds (the kernel's authoritative `now_ms`),
/// `kind` a short category tag (`"gate"`, `"research"`, `"task"`, `"phase"`, ...), and `summary`
/// a one-line human description that NEVER carries document content (byte counts / ids / reasons
/// only). Rendered on the panel's trace tab as `HH:MM:SS kind summary`.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct TraceEvent {
    pub ts: i64,
    pub kind: String,
    pub summary: String,
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
    /// How the safeguard handles flagged assumptions once the checker returns (autonomous-safeguard
    /// pivot 2026-07-15). `"auto"` (default) = ULTRA-AUTOMATIC: only `[critical]` items freeze the
    /// pipeline for the human; everything else is auto-resolved by the office ("research, decide,
    /// disclose") over a bounded round loop and the pipeline never stalls on paperwork. `"ask"` =
    /// the original freeze-and-ask behavior for EVERY material item. `assumption_check == false`
    /// still disables the checker entirely regardless of mode. Named-fn default `"auto"` (not
    /// `#[serde(default)]`, which would force an empty string) so legacy state files load autonomous.
    /// (Unification 2026-07-15: this single mode enum SUPERSEDES the branch's `assumption_trust`
    /// bool — `"auto"` is the old `assumption_trust == true`, `"ask"` the old `false`; the new
    /// default flips to autonomous.)
    #[serde(default = "default_assumption_mode")]
    pub assumption_mode: String,
    /// Whether — and how eagerly — the drafting pipeline runs the web-research analyst (design-
    /// speedup): `"always"` = always research (the pre-speedup behavior), `"never"` = skip research
    /// entirely (trace `research skipped (config)`), `"auto"` (default) = ask the PRD safeguard-gate
    /// model one extra boolean (is the stack entirely mainstream/well-known?) and skip research when
    /// it says yes. Only those three values are accepted; any other is ignored like an absent field.
    /// Named-fn default `"auto"` (not `#[serde(default)]`, which would force an empty string) so
    /// legacy state files load with the auto policy.
    #[serde(default = "default_research_mode")]
    pub research_mode: String,
    /// Optional model override for the DOC-DRAFTING invokes only (design-speedup): the PRD persona
    /// reply, the combined TRD+CRD authoring invoke, and the ask-mode auto-resolve rewrite. When
    /// `Some`, those invokes carry it as the `models.invoke` `model` param (mirroring how
    /// `worker_model`/`reviewer_model` ride `sessions.spawn_into`); the gate/safeguard checks keep
    /// resolving against `safeguard_role` with no override. `None` (default) = every invoke resolves
    /// its role's model as before. `#[serde(default)]` (None) for back-compat.
    #[serde(default)]
    pub drafter_model: Option<String>,
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

fn default_assumption_mode() -> String {
    "auto".to_string()
}

fn default_research_mode() -> String {
    "auto".to_string()
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
            assumption_mode: default_assumption_mode(),
            research_mode: default_research_mode(),
            drafter_model: None,
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
    /// Ungrounded assumptions the safeguard flagged in the LAST doc gate that STOP the pipeline for
    /// the human (6.2c/safeguard). In `assumption_mode == "ask"` this is every material item; in
    /// `"auto"` it is ONLY the `[critical]` items (auto items are resolved autonomously and never
    /// persisted here). Non-empty means the pipeline is stopped waiting on the user to
    /// approve/answer/delegate; a subsequent clean check on any doc clears it. `#[serde(default)]`
    /// for back-compat.
    #[serde(default)]
    pub pending_assumptions: Vec<String>,
    /// Sticky per-project approval of the safeguard gate. Set once the user answers a
    /// pending-assumptions stop with a deterministic approval intent (kernel `office_message`) OR
    /// invokes `workflow_approve` (kernel `approve_assumptions`); thereafter `gate_doc` fails OPEN
    /// for every doc in THIS project (`assumptions_approved || !config.assumption_check`), so a
    /// re-emitted doc proceeds instead of re-stopping (the audit's approval loop). NEVER auto-reset
    /// — the owner's "super-autonomous once approved" contract: once set it dominates, so toggling
    /// `config.assumption_check` does NOT re-gate this project. `config.assumption_check` remains
    /// the wholesale gate on/off escape hatch for projects that were never approved.
    /// `#[serde(default)]` (false) for back-compat.
    #[serde(default)]
    pub assumptions_approved: bool,
    /// Append-only audit trail of assumptions the safeguard flagged that an active approval
    /// (`assumptions_approved`, set by chat intent or `workflow_approve`) auto-resolved instead of
    /// stopping on. Capped to the most recent ~100 entries so a long drafting session can never
    /// balloon the state file. `#[serde(default)]` for back-compat.
    #[serde(default)]
    pub self_resolved_assumptions: Vec<String>,
    /// Consecutive deterministic capture-miss nudges issued for the current PRD (kernel Persona
    /// arm): incremented each time a long Drafting reply lands with no ```prd fence while the PRD
    /// slot is still empty, reset to 0 on a successful fence capture. Caps the nudge loop
    /// (`MAX_CAPTURE_NUDGES`) so a model that never emits the fence falls back to waiting on the
    /// user rather than looping forever. `#[serde(default)]` (0) for back-compat.
    #[serde(default)]
    pub capture_nudge_count: u32,
    /// How many autonomous auto-resolution rounds the safeguard has run on the CURRENT doc capture
    /// (autonomous-safeguard pivot). Bumped each time an `InvokePurpose::AssumeResolve` invoke is
    /// emitted, RESET to 0 on every fresh doc capture (a persona/TRD/CRD fence), and capped at
    /// `AUTO_ROUND_CAP` (2) — after the cap the pipeline proceeds anyway with the undecided items
    /// documented in the doc (ultra-automatic mode never stalls). Persisted so the round budget
    /// survives a store reload. `#[serde(default)]` (0) for back-compat.
    #[serde(default)]
    pub assumption_rounds: u32,
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
    /// Machine-diary trace ring (feature: tracelog): a capped, newest-last log of what the office
    /// machine did — persona/TRD/CRD invokes, doc captures, safeguard-gate stops/approvals,
    /// research + audit lifecycle, breakdown/authorize, task transitions, interrupt/resume. One
    /// line per entry, never document content. Capped at the kernel's `TRACE_CAP` (200, oldest
    /// dropped) so a long project can never balloon `state.json`. `#[serde(default)]` for
    /// back-compat with pre-tracelog state files.
    #[serde(default)]
    pub trace: Vec<TraceEvent>,
    /// The phase a currently-`Interrupted` project was in when it was interrupted (feature:
    /// interrupt-from-drafting). Set on interrupt, read on resume so the machine returns to the
    /// RIGHT phase — a Drafting-interrupt resumes to `Drafting`, a Running/Halted one to `Running`
    /// — then cleared. `None` whenever the project is not interrupted. `#[serde(default)]` so
    /// pre-feature state files (which never encode it) still load clean.
    #[serde(default)]
    pub interrupted_from: Option<ProjectPhase>,
    /// Whether the CURRENT drafting doc-set's safeguard gate has cleared (design-speedup one-shot
    /// gate + parallel joins). Because the pipeline authors PRD then TRD+CRD strictly in order and
    /// each fresh capture resets this, ONE flag serves both join points: during the PRD stage it
    /// records "PRD gate cleared" (the research join in `maybe_author_trdcrd`); during the TRD+CRD
    /// stage it records "TRD+CRD gate cleared" (the breakdown join in `maybe_apply_breakdown`). Reset
    /// to `false` on every fresh doc capture (PRD or TRD+CRD). `#[serde(default)]` (false) for
    /// back-compat.
    #[serde(default)]
    pub gate_cleared: bool,
    /// A validated epic/story/task breakdown computed EARLY — as soon as the TRD is captured, in
    /// parallel with the TRD+CRD gate verify (design-speedup item 8) — and stashed here as its raw
    /// (already-parsed-once) model text until the gate clears, at which point it is re-parsed and
    /// applied to build the board (`maybe_apply_breakdown`, Drafting -> Ready). Discarded on a fresh
    /// TRD+CRD capture (a revised TRD invalidates the stale plan — "breakdown redone"). Raw text
    /// rather than the parsed `office::Breakdown` so the domain layer need not depend on `office`.
    /// `#[serde(default)]` (None) for back-compat.
    #[serde(default)]
    pub pending_breakdown: Option<String>,
    /// Runtime-only hint (review finding, migration self-heal): "a PRD gate (`AssumeCheckPrd` /
    /// resolve / verify) invoke was fired by THIS process and may still be in flight". Set `true`
    /// at every PRD-stage invoke emission (`gate_doc`, `emit_resolve`, `emit_verify`, each gated on
    /// `Deferred::PostPrd`) and `false` at every terminal PRD-stage outcome (`run_gate_cleared`'s
    /// PostPrd arm covers clean/fail-open/approved/verified; `freeze_critical` covers the
    /// critical-freeze arm). Deliberately `#[serde(skip)]` — an in-flight invoke can never survive
    /// a daemon restart or lease transfer, so it is NOT persisted and always deserializes to
    /// `false`. That is exactly what makes it the authoritative "no gate outcome can ever arrive"
    /// signal for [`self_heal_stale_prd_gate`] in kernel.rs: a project freshly loaded from disk has
    /// this `false` regardless of what a pre-migration build's in-memory state looked like, so a
    /// settling research binding heals the gate on first settle instead of wedging forever.
    #[serde(skip)]
    pub gate_invoke_live_hint: bool,
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
