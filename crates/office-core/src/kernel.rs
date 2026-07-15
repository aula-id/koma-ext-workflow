//! The deterministic kernel (ARCHITECTURE.md 5.1-5.3).
//!
//! `kernel::step` is a pure function over a single `Project`: it consumes one
//! `Input` (a panel/tool `Command` or a `HostEvent` fed back by the driver),
//! mutates the project's in-memory state, and returns a `Vec<Effect>` describing
//! the side effects the driver must perform. It never does IO, never calls the
//! host, and never blocks on a model — "no LLM in the control loop" means the
//! kernel only ever *emits* `InvokeModel` requests and *consumes* their results as
//! ordinary commands (wired in W9).
//!
//! Purity is what makes it testable: every branch below is exercised with
//! deterministic inputs in `kernel_test.rs`, and `step` run twice on identical
//! state yields an identical effect vector.
//!
//! ## Two-phase spawn
//! The kernel cannot mint host agent ids, so a dispatch is two-phase. On the
//! dispatch scan it emits a `Spawn` effect and immediately moves the task into
//! `OnProgress`/`Review` with a *provisional* binding (`ext_agent_id == 0`). The
//! driver executes the spawn, learns the real agent id, and feeds a
//! `HostEvent::Spawned` back which records the id onto that binding. Parking the
//! task in an in-flight state on the same tick (rather than leaving it `Todo`)
//! keeps the ready-set from double-dispatching it before the id returns.
//!
//! ## Counters live in `Task::history`
//! The domain `Task` deliberately carries no `attempt`/spawn-failure fields, so the
//! kernel keeps those ledgers in `history` as tagged events and derives them with
//! the helpers below (`next_attempt`, `spawn_failure_streak`). `bounces` is the one
//! counter the domain models directly, and it is used verbatim.
//!
//! ## Drafting pipeline (design-speedup 2026-07-15)
//! Drafting is a PARALLEL pipeline with two doc-sets and two JOINs:
//! `PRD -> {research ∥ PRD gate} -join-> TRD+CRD -> {early breakdown ∥ TRD+CRD gate} -join-> Ready`.
//! At PRD capture the researcher is spawned (per `research_mode`, item 4) CONCURRENTLY with the PRD
//! safeguard gate; the TRD+CRD authoring invoke fires only once BOTH settle (`maybe_author_trdcrd`).
//! One combined invoke authors BOTH docs (item 3), gated together by the single TRD+CRD gate. When
//! that gate finalizes the TRD it starts the epic/story/task breakdown EARLY, in parallel with the
//! gate's verify; the breakdown is stashed and applied (Drafting -> Ready) at the second JOIN
//! (`maybe_apply_breakdown`, item 8). One shared `gate_cleared` flag serves both JOINs because the
//! stages are strictly sequential and every fresh capture resets it.
//!
//! Each gate is the ONE-SHOT safeguard (item 5 + amendment A): in `assumption_mode == "auto"` the
//! ENUMERATE pass ALSO resolves the non-critical items inline and re-emits the revised doc(s), then
//! a single VERIFY pass may only CLEAR or DISCLOSE — so a clean doc is one invoke, a dirty doc two,
//! never a loop. `ask` mode keeps enumerate as its own pass (critical items surface before any
//! rewrite), then resolves the non-critical remainder + verifies. Which doc-set a gate belongs to
//! is a pure function of which docs are non-empty (`newest_gated_doc`), so nothing "which stage"
//! needs persisting. A stopped gate (critical freeze) re-enters via a re-emitted fence, a fenceless
//! reply while `pending_assumptions` is set (`recheck_pending_assumptions`), or `ApproveAssumptions`.
//! An interrupted Drafting project respawns its researcher on resume (item 6); `workflow_skip`
//! (item 7) cancels research and advances the join. `drafter_model` (item 4) overrides the model on
//! the doc-drafting invokes only.
//!
//! ## Completion audit gate (6.2c)
//! When the last task passes and the project would complete, [`maybe_complete_project`] spawns
//! the project-level clean-build auditor (`Project.audit`, two-phase + reconcile-covered exactly
//! like `research`) INSTEAD of going Done, whenever a CRD exists. The audit grade then gates Done
//! vs. up to two automated remediation rounds vs. a parked task (`audit_rounds`, persisted). Every
//! failure mode degrades to Done — a missing CRD, a dead/timed-out auditor, or an unparseable
//! grade never wedges completion.

use std::path::{Path, PathBuf};

use crate::domain::{
    AgentBinding, AgentKind, ChatAuthor, ChatMsg, Comment, CommentAuthor, CommentId, ParkReason,
    Project, ProjectPhase, Receipt, Task, TaskEvent, TaskId, TaskState, TraceEvent,
};
use crate::graph::{self, ready_set};
use crate::machine::{step_project, ProjectTransition};
use crate::office::{self, InvokePurpose};
use crate::prompts;
use crate::report::{self, ReportStatus, Verdict};

/// Sentinel agent id for a provisional binding whose real id has not returned from
/// the host yet. `0` is never a real koma agent id.
const PROVISIONAL: u64 = 0;

/// The office self-caps its concurrent sub-agents so one host slot is always left
/// for the user (host `MAX_SUBAGENTS == 5`; ARCHITECTURE.md 5.2.3). The driver
/// threads the *remaining* session-global slot count into `step` as
/// `session_capacity`; this constant is only the clamp ceiling for the per-project
/// `max_workers` soft sub-ceiling.
const MAX_PROJECT_WORKERS: u32 = 4;

/// Max consecutive capture-miss nudges the kernel fires for the PRD before falling back to waiting
/// on the user (feature: capture nudge). A long Drafting reply with no ```prd fence and no PRD yet
/// triggers a deterministic re-invoke asking for ONLY the fenced doc; after this many in a row
/// (reset by any successful capture) the kernel stops nudging and surfaces the reply as today.
const MAX_CAPTURE_NUDGES: u32 = 2;

/// A Drafting reply must be at least this many bytes to be treated as a forgotten-fence PRD worth
/// nudging (feature: capture nudge). Short fence-less replies are almost always legitimate
/// clarifying questions the office is waiting on the user to answer, so nudging them would wrongly
/// force a premature PRD; only a long prose reply is likely a PRD the office narrated but forgot to
/// wrap in the ```prd fence (live-test 2026-07-15).
const PRD_NUDGE_MIN_REPLY_BYTES: usize = 500;

/// The system-appended instruction on a capture-miss nudge re-invoke (feature: capture nudge).
const PRD_NUDGE_INSTRUCTION: &str = "\nYour previous reply did not include the required ```prd \
fence. Emit ONLY the complete document in the fence now — no prose.\n";

/// The system-appended instruction on a combined TRD+CRD capture-miss nudge (design-speedup item 3):
/// the reply dropped one or both fences, so re-ask for BOTH, fenced, nothing else.
const TRDCRD_NUDGE_INSTRUCTION: &str = "\nYour previous reply was missing the required ```trd and/or \
```crd fence. Emit ONLY the two complete documents, each in its own fence (```trd ... ``` then \
```crd ... ```), nothing else.\n";

/// The per-doc fence tags of each drafting doc-set, passed to the gate/resolve prompt builders.
const PRD_TAGS: &[&str] = &["prd"];
const TRDCRD_TAGS: &[&str] = &["trd", "crd"];

/// The system-appended instruction on an ENHANCEMENT change-brief capture-miss nudge (feature:
/// sdlc-triage): the reply dropped the ```change fence, so re-ask for ONLY the fenced change-brief.
const CHANGE_NUDGE_INSTRUCTION: &str = "\nYour previous reply did not include the required ```change \
fence. Emit ONLY the complete change-brief in the fence now (Current behavior / Desired behavior / \
Acceptance criteria) — no prose.\n";

/// SDLC escalation threshold (feature: sdlc-triage): an ENHANCEMENT whose change-brief gate surfaces
/// MORE than this many material assumptions is wider than a change and escalates to the full project
/// track. N = 4 (documented in requirement 3).
const ENHANCEMENT_ASSUMPTION_ESCALATION_MAX: usize = 4;

/// SDLC escalation threshold (feature: sdlc-triage): an ENHANCEMENT whose breakdown returns MORE
/// than this many tasks is wider than a change and escalates to the full project track. The small
/// breakdown prompt already caps at 3; this catches a model that overshoots anyway.
const ENHANCEMENT_BREAKDOWN_TASK_MAX: usize = 3;

/// SDLC escalation trigger (feature: sdlc-triage): a PATCH whose single task reaches this many
/// bounces is wider than a patch and escalates (converts) to the enhancement track before its next
/// re-dispatch. "The single task bounces twice" -> escalate on the 2nd bounce.
const PATCH_BOUNCE_ESCALATION: u32 = 2;

// ---------------------------------------------------------------------------
// Public protocol
// ---------------------------------------------------------------------------

/// A single input to the kernel: either a user/tool intent (`Command`) or a fact
/// the host reported (`HostEvent`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Input {
    Command(Command),
    Host(HostEvent),
}

/// Intents from the panel, a contributed tool call, an inbox file, or an office
/// decision. W4 implements the control-loop subset; PRD/breakdown/authorize
/// commands arrive in W9.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Command {
    /// Emergency brake. `hard` (the default board button) kills every tracked
    /// binding and normalizes in-flight tasks; soft drain merely stops dispatch and
    /// lets in-flight agents finish (5.5).
    Interrupt { hard: bool },
    /// Interrupted/Halted -> Running; the next dispatch scan re-arms the line.
    Resume,
    /// Parked -> Todo (attempt preserved).
    Unpark { task: TaskId },
    /// Append a board comment (starts `Pending`; delivered when folded into a spawn).
    AddComment {
        task: TaskId,
        author: CommentAuthor,
        text: String,
    },
    /// A user message to the front office (panel chat / `workflow_brief` / inbox brief).
    /// Appends the turn to the transcript and issues a persona invoke (folding first if
    /// the assembled prompt would cross the threshold, 6.2). Off-loop: the kernel only
    /// emits the `InvokeModel` effect; the driver runs it.
    OfficeMessage { text: String },
    /// Ask the office to author the epic/story/task breakdown for the current PRD
    /// (6.3.2). Emits a breakdown invoke; the result is validated on arrival.
    RequestBreakdown,
    /// Authorize `Ready -> Running` with a delivery path the driver has already
    /// validated + `mkdir -p`'d (6.3.3). `delivery_valid` is the driver's containment
    /// verdict; a false verdict never transitions (the hard gate). `worktree` is the driver's
    /// git-repo setup verdict (item 1): `Ok(())` = the delivery is a git repo and worktree desks
    /// are on; `Err(reason)` = `git` was missing or `init` failed, so the project falls back to
    /// legacy copy-desks (the kernel records the flag + traces the reason).
    Authorize {
        delivery_path: PathBuf,
        allow_outside_workspace: bool,
        worktree: Result<(), String>,
    },
    /// An off-loop `models.invoke` returned (5.1). `purpose` says which flow it belongs
    /// to; `outcome` is the model text or the error string after the driver's one retry.
    /// This is the concrete "consume invoke results as ordinary commands": the kernel
    /// applies each result deterministically and never blocks on the model.
    InvokeResult {
        purpose: InvokePurpose,
        outcome: Result<String, String>,
    },
    /// Panel `config_set` (ARCHITECTURE.md 10.2 / PANEL_PROTOCOL.md 1.2): a direct,
    /// synchronous edit of `ProjectConfig`. Absent fields leave the current value
    /// untouched (partial update); there is no state-machine transition or dispatch
    /// consequence beyond the fields themselves.
    ConfigSet {
        max_workers: Option<u32>,
        bounce_budget: Option<u32>,
        worker_model: Option<String>,
        reviewer_model: Option<String>,
        keep_desks: Option<bool>,
        crd_pass_grade: Option<u32>,
        assumption_check: Option<bool>,
        /// The safeguard assumption-handling mode: `"auto"` (autonomous) | `"ask"` (freeze-and-ask).
        /// Only those two values are accepted (case-insensitive); any other string is ignored like
        /// an absent field (additive partial update). Autonomous-safeguard pivot 2026-07-15.
        /// (Unification 2026-07-15: the single knob that supersedes the branch's `assumption_trust`
        /// bool — `"auto"` = old trust ON, `"ask"` = old trust OFF.)
        assumption_mode: Option<String>,
        /// The research policy (design-speedup item 4): `"auto"` | `"always"` | `"never"`. Only those
        /// three values are accepted (case-insensitive); any other string is ignored like an absent
        /// field, so a typo never silently changes behavior.
        research_mode: Option<String>,
        /// The doc-drafting model override (design-speedup item 4). `Some("")` CLEARS the override
        /// back to `None` (resolve the role's model); any other `Some(m)` sets it; `None` leaves it
        /// unchanged (additive partial update).
        drafter_model: Option<String>,
    },
    /// Explicit human approval of the safeguard's pending assumptions (`workflow_approve`,
    /// ARCHITECTURE.md 6.2c). Records the approval in the office transcript, then — human approval
    /// OUTRANKING the checker — clears `pending_assumptions` directly and resumes the deferred
    /// drafting stage for the newest gated doc, WITHOUT waiting on another safeguard invoke. A
    /// no-op notice when nothing is pending.
    ApproveAssumptions,
    /// User-driven "skip research" (design-speedup item 7, `workflow_skip`). When the web-research
    /// analyst is in flight during Drafting, kill it, mark research skipped (empty notes are fine),
    /// and advance the pipeline toward the TRD+CRD authoring join. When no research is in flight,
    /// it is a no-op beyond a friendly notice naming the project's current phase.
    SkipResearch,
}

/// Facts fed back by the driver. `Tick`/`Reconcile` carry no clock — the authoritative
/// clock is the `now_ms` argument to `step`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum HostEvent {
    /// Periodic wake (driver `recv_timeout`). Drives the dispatch scan.
    Tick,
    /// Rate-limited reconcile pass. Drives the per-worker runtime ceiling (5.2.4).
    Reconcile,
    /// The driver executed a `Spawn` and learned the real agent id.
    Spawned {
        task: TaskId,
        agent_id: u64,
        spawned_at_ms: u64,
    },
    /// A sub-agent reached a terminal host status (`done`/`error`/`killed`). `error` is the
    /// optional additive failure text koma now sends alongside a non-`done` status (feature C);
    /// `None` when absent (old komas) or on the driver's own `agents.status`-poll path, so the
    /// event shape stays back-compatible.
    AgentsDone { agent_id: u64, status: String, error: Option<String> },
    /// The driver fetched a terminal agent's report/review text.
    Result { agent_id: u64, text: String },
    /// A `Spawn` effect failed before producing any report.
    SpawnFailed { task: TaskId, reason: String },
    /// The driver executed a `SpawnResearch` and learned the real research agent id (6.2b).
    ResearchSpawned { agent_id: u64, spawned_at_ms: u64 },
    /// A `SpawnResearch` failed before producing any findings — grant denied, unknown agent,
    /// capacity, or a cross-process `{status:"sent"}` reply (6.2b). Drafting degrades to a
    /// PRD-only TRD; a dead/hung researcher (killed path, runtime ceiling) degrades the same way.
    ResearchFailed { reason: String },
    /// The driver executed a `SpawnAudit` and learned the real auditor agent id (6.2c).
    AuditSpawned { agent_id: u64, spawned_at_ms: u64 },
    /// A `SpawnAudit` failed before producing a verdict — grant denied, unknown agent, capacity,
    /// a cross-process `{status:"sent"}` reply, a dead/killed auditor, or the runtime ceiling
    /// (6.2c). The project degrades to Done WITHOUT an audit (never wedges completion).
    AuditFailed { reason: String },
    /// The driver's `agents.send` for a mid-run comment injection (feature 4) succeeded
    /// (`{"sent":true}` / `{"sent":true,"status":"queued"}`). Flips that comment's receipt
    /// `Pending -> Delivered`. Only emitted on success; an `agents.send` error produces no
    /// event, leaving the comment `Pending` for the spawn-boundary fold to deliver later.
    CommentDelivered { task: TaskId, comment_id: CommentId },
    /// Worktree desks (item 1): the driver merged a task's branch cleanly into main (in response
    /// to `Effect::MergeDesk`). The task completes (Done) and its worktree is reclaimed.
    DeskMerged { task: TaskId },
    /// Worktree desks (item 1): the driver's merge of a task's branch hit a conflict, or failed for
    /// some other reason. `summary` is the conflict/error description; `is_conflict` (item 4)
    /// distinguishes a REAL content conflict (wording tells the user to resolve it) from any other
    /// merge failure (wording just says the task was re-queued). Either way the task bounces with the
    /// summary as the review note and a retry gets a fresh worktree branched off the now-advanced
    /// main.
    DeskMergeConflict {
        task: TaskId,
        summary: String,
        is_conflict: bool,
    },
}

/// Side effects for the driver to execute. `InvokeModel`/`PublishContext` are part
/// of the frozen protocol but are emitted by the driver/office in W7/W9, not by the
/// W4 control loop.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Effect {
    Spawn {
        task: TaskId,
        prompt: String,
        /// The sub-agent id to spawn. A worker dispatch carries the task's persona id
        /// (`office-worker-<name>`, deterministically chosen by `persona::worker_agent_id`
        /// over the task id); a reviewer dispatch carries the fixed `office-reviewer`.
        /// Owned `String` (was `&'static str`) so it can carry the per-task persona.
        agent: String,
        model: Option<String>,
    },
    /// Spawn the project-level `office-researcher` (ARCHITECTURE.md 6.2b). Two-phase like
    /// `Spawn`: the driver runs it via the SAME `sessions.spawn_into` path and feeds the real
    /// agent id back as `HostEvent::ResearchSpawned` (or `ResearchFailed`). No task/model — it
    /// is a one-shot analyst on the whole PRD, inheriting the office role's model.
    SpawnResearch {
        prompt: String,
    },
    /// Spawn the project-level `office-auditor` at completion (ARCHITECTURE.md 6.2c). Two-phase
    /// like `SpawnResearch`: the driver runs it via the SAME `sessions.spawn_into` path and feeds
    /// the real agent id back as `HostEvent::AuditSpawned` (or `AuditFailed`, which degrades the
    /// project to Done without an audit). Read-only auditor persona; inherits the office model.
    SpawnAudit {
        prompt: String,
    },
    Kill {
        ext_agent_id: u64,
    },
    FetchResult {
        ext_agent_id: u64,
    },
    /// Deliver a board comment to a LIVE sub-agent mid-run via the host `agents.send` verb
    /// (feature 4). Emitted by `AddComment` only when the task carries a real (non-provisional)
    /// binding — an in-flight worker (`OnProgress`) or a spawned reviewer (`Review` with a
    /// reviewer binding). `text` is the framed injection line the agent acks in its report. The
    /// comment stays `Pending` until the driver reports the send succeeded (feeding back
    /// `HostEvent::CommentDelivered`); on any `agents.send` error the driver drops it and the
    /// existing spawn-boundary fold (`spawn_worker`) delivers it on the next attempt — one shot
    /// per comment at add time, no retry loop.
    InjectComment {
        ext_agent_id: u64,
        comment_id: CommentId,
        text: String,
    },
    InvokeModel {
        req_id: u64,
        purpose: InvokePurpose,
        role: String,
        /// Optional `models.invoke` `model` override (design-speedup item 4, `drafter_model`). `Some`
        /// only on the doc-drafting invokes (persona / TRD+CRD / ask-mode auto-resolve) when the
        /// project set a `drafter_model`; the driver forwards it as the `model` param when `Some`,
        /// exactly as `worker_model`/`reviewer_model` ride `sessions.spawn_into`. `None` = resolve
        /// the role's model as before.
        model: Option<String>,
        system: String,
        prompt: String,
        /// The `models.invoke` output format (feature 5): `Some("json")` for the structured
        /// invokes (breakdown family + assume-check gate) maps host-side to a chat-completions
        /// `response_format: json_object`; other dialects silently ignore it. `None` for the
        /// prose invokes (persona/TRD/CRD/fold). Set by [`invoke_format`]; the driver forwards it
        /// as the `models.invoke` `format` param only when `Some`.
        format: Option<&'static str>,
    },
    PublishContext {
        text: String,
    },
    QueueChatPrompt {
        notice_id: u64,
        text: String,
    },
    PanelPush {
        snapshot: bool,
    },
    EnsureDesk {
        task: TaskId,
        dir: PathBuf,
    },
    /// Worktree desks (item 1/2): merge a task's `task/<slug>` branch into the delivery repo's
    /// main branch after review PASS. The driver runs the git merge (serialized per repo,
    /// fast-forward preferred) and feeds back `HostEvent::DeskMerged` (clean) or
    /// `HostEvent::DeskMergeConflict` (conflict or other failure -> the task bounces with the
    /// summary, worded per `is_conflict`, item 4).
    MergeDesk {
        task: TaskId,
        /// The delivery path — the git repo whose main branch receives the merge.
        repo: PathBuf,
        /// The task's worktree path (removed after a clean merge).
        desk: PathBuf,
        /// The task branch `task/<slug>`.
        branch: String,
    },
    /// Worktree desks (item 1): remove a task's worktree + delete its branch. Fire-and-forget
    /// best-effort (no feedback event) — emitted on Done and on a terminal park when `keep_desks`
    /// is off. A retry re-materializes a FRESH worktree regardless (the add tears down any stale
    /// one first), so a bounce-within-budget relies on that rather than this.
    RemoveDesk {
        repo: PathBuf,
        desk: PathBuf,
        branch: String,
    },
    Persist,
}

// ---------------------------------------------------------------------------
// Entry point
// ---------------------------------------------------------------------------

/// Advance a project by one input. `session_capacity` is the session-global number
/// of office sub-agent slots still free — owned by the driver across every project
/// it leases (5.2.3), NOT computed per-project. Returns the effects to execute in
/// order; a `Persist` + `PanelPush` pair is appended once whenever the input mutated
/// state.
pub fn step(p: &mut Project, input: Input, now_ms: u64, session_capacity: u32) -> Vec<Effect> {
    let mut ctx = Ctx {
        fx: Vec::new(),
        dirty: false,
    };

    match input {
        Input::Command(c) => handle_command(p, c, now_ms, &mut ctx),
        Input::Host(e) => handle_event(p, e, now_ms, &mut ctx),
    }

    // Dispatch runs after every input, but only while the project is actively
    // Running (Interrupted/Halted/Done stop the line; in-flight results still get
    // processed above so a soft drain completes naturally).
    if matches!(p.phase, ProjectPhase::Running) {
        dispatch(p, now_ms, session_capacity, &mut ctx);
    }

    if ctx.dirty {
        ctx.fx.push(Effect::Persist);
        ctx.fx.push(Effect::PanelPush { snapshot: true });
    }

    ctx.fx
}

/// Effect accumulator threaded through the handlers. `dirty` records whether any
/// state mutation happened so the trailing `Persist`/`PanelPush` is emitted exactly
/// once per input.
struct Ctx {
    fx: Vec<Effect>,
    dirty: bool,
}

// ---------------------------------------------------------------------------
// Command handling
// ---------------------------------------------------------------------------

fn handle_command(p: &mut Project, c: Command, now_ms: u64, ctx: &mut Ctx) {
    match c {
        Command::Interrupt { hard } => {
            if hard {
                hard_interrupt(p, now_ms, ctx);
            } else {
                soft_interrupt(p, now_ms, ctx);
            }
        }
        Command::Resume => {
            // The resume target is remembered on `interrupted_from` (set at interrupt time): a
            // Drafting-interrupt resumes back to Drafting, everything else to Running. The machine
            // owns the actual edge; the kernel only supplies the recalled flag.
            let to_drafting = matches!(p.interrupted_from, Some(ProjectPhase::Drafting));
            if let Ok(ph) = step_project(&p.phase, ProjectTransition::Resume { to_drafting }) {
                trace(p, now_ms, "phase", format!("resumed to {}", phase_label(&ph)), ctx);
                p.phase = ph;
                p.interrupted_from = None;
                // Design-speedup item 6: a hard interrupt during Drafting KILLED any in-flight
                // researcher; resuming would otherwise wait for a user message before the pipeline
                // moved again. If the project is back in Drafting with a captured PRD, no research
                // notes yet, no researcher in flight, research not disabled, and TRD+CRD not yet
                // authored, respawn the researcher immediately so drafting continues on its own.
                if resume_should_respawn_research(p) {
                    trace(p, now_ms, "research", "respawned on resume", ctx);
                    start_research(p, now_ms, ctx);
                }
            }
        }
        Command::Unpark { task } => {
            if let Some(idx) = find_task(p, &task) {
                if let TaskState::Parked { attempt, .. } = p.tasks[idx].state {
                    set_next_attempt(&mut p.tasks[idx], now_ms, attempt);
                    record(&mut p.tasks[idx], now_ms, "unparked");
                    p.tasks[idx].state = TaskState::Todo;
                    ctx.dirty = true;
                }
            }
        }
        Command::AddComment {
            task,
            author,
            text,
        } => {
            if let Some(idx) = find_task(p, &task) {
                let id = CommentId(
                    p.tasks[idx]
                        .comments
                        .iter()
                        .map(|c| c.id.0)
                        .max()
                        .unwrap_or(0)
                        + 1,
                );
                // If the task carries a LIVE (real-id) binding, push the comment to the running
                // agent mid-run via `agents.send` (feature 4). Built BEFORE the comment is moved
                // so the frame can borrow `text`. A provisional (id 0) or bindingless state has no
                // reachable agent -> None, and the comment waits Pending for the spawn-boundary
                // fold instead.
                let inject = live_binding_id(&p.tasks[idx].state).map(|ext_agent_id| {
                    Effect::InjectComment {
                        ext_agent_id,
                        comment_id: id,
                        text: format!(
                            "[user comment c{}] {}\nAcknowledge in your OFFICE-REPORT ack-comments.",
                            id.0, text
                        ),
                    }
                });
                p.tasks[idx].comments.push(Comment {
                    id,
                    author,
                    text,
                    created_ms: now_ms,
                    receipt: Receipt::Pending,
                });
                if let Some(fx) = inject {
                    ctx.fx.push(fx);
                }
                ctx.dirty = true;
            }
        }
        Command::OfficeMessage { text } => office_message(p, text, now_ms, ctx),
        Command::RequestBreakdown => request_breakdown(p, now_ms, ctx),
        Command::Authorize {
            delivery_path,
            allow_outside_workspace,
            worktree,
        } => authorize(p, delivery_path, allow_outside_workspace, worktree, now_ms, ctx),
        Command::InvokeResult { purpose, outcome } => {
            invoke_result(p, purpose, outcome, now_ms, ctx)
        }
        Command::ConfigSet {
            max_workers,
            bounce_budget,
            worker_model,
            reviewer_model,
            keep_desks,
            crd_pass_grade,
            assumption_check,
            assumption_mode,
            research_mode,
            drafter_model,
        } => {
            if let Some(w) = max_workers {
                p.config.max_workers = w.clamp(1, MAX_PROJECT_WORKERS);
                ctx.dirty = true;
            }
            if let Some(b) = bounce_budget {
                p.config.bounce_budget = b;
                ctx.dirty = true;
            }
            if let Some(m) = worker_model {
                p.config.worker_model = Some(m);
                ctx.dirty = true;
            }
            if let Some(m) = reviewer_model {
                p.config.reviewer_model = Some(m);
                ctx.dirty = true;
            }
            if let Some(k) = keep_desks {
                p.config.keep_desks = k;
                ctx.dirty = true;
            }
            if let Some(g) = crd_pass_grade {
                // Clamp to a valid rubric grade; 0 disables the gate in effect (any audit passes).
                p.config.crd_pass_grade = g.min(100);
                ctx.dirty = true;
            }
            if let Some(a) = assumption_check {
                p.config.assumption_check = a;
                ctx.dirty = true;
            }
            if let Some(m) = assumption_mode {
                // Accept only the two known modes (case-insensitive); any other value is ignored
                // like an absent field, so a typo never silently changes behavior.
                match m.trim().to_ascii_lowercase().as_str() {
                    "auto" => {
                        p.config.assumption_mode = "auto".to_string();
                        ctx.dirty = true;
                    }
                    "ask" => {
                        p.config.assumption_mode = "ask".to_string();
                        ctx.dirty = true;
                    }
                    _ => {}
                }
            }
            if let Some(m) = research_mode {
                // Accept only the three known policies (case-insensitive); any other value is
                // ignored like an absent field (design-speedup item 4).
                match m.trim().to_ascii_lowercase().as_str() {
                    "auto" => {
                        p.config.research_mode = "auto".to_string();
                        ctx.dirty = true;
                    }
                    "always" => {
                        p.config.research_mode = "always".to_string();
                        ctx.dirty = true;
                    }
                    "never" => {
                        p.config.research_mode = "never".to_string();
                        ctx.dirty = true;
                    }
                    _ => {}
                }
            }
            if let Some(m) = drafter_model {
                // An empty string clears the override back to None (resolve the role's model); any
                // other value sets it (design-speedup item 4).
                p.config.drafter_model = if m.trim().is_empty() { None } else { Some(m) };
                ctx.dirty = true;
            }
        }
        Command::ApproveAssumptions => approve_assumptions(p, now_ms, ctx),
        Command::SkipResearch => skip_research(p, now_ms, ctx),
    }
}

// ---------------------------------------------------------------------------
// Front office (6.2 / 6.3) — off-loop invoke choreography
// ---------------------------------------------------------------------------

/// Deterministic approval-intent phrases (matched case-insensitively, whole-word/phrase). When a
/// project is stopped on `pending_assumptions` and the user's message carries one of these, the
/// safeguard gate is closed for the project so the re-emitted doc proceeds instead of re-stopping
/// (the audit's approval loop). Kept explicit + auditable rather than a fuzzy classifier.
const APPROVAL_PHRASES: &[&str] = &[
    "approve", "approved", "you decide", "go ahead", "proceed", "lgtm", "ok go",
];

/// Negation words that VETO an approval match. A message that pairs an approval word with any of
/// these is ambiguous ("I don't approve of waiting"), so it is conservatively NOT treated as
/// approval — the safeguard is a SAFETY gate, so only a CLEAR approval closes it (an owner who
/// wants blanket autonomy sets `config.assumption_mode = "auto"` instead). Matched whole-word after
/// folding apostrophes ("don't" -> "dont"), so "another" never reads as "not".
const APPROVAL_NEGATIONS: &[&str] = &[
    "not", "dont", "never", "cant", "cannot", "wont", "reject", "disapprove",
];

/// Normalize `msg` for whole-word/phrase matching: lowercase, drop apostrophes (so "don't"
/// folds to one token "dont"), replace every other non-alphanumeric run with a single space,
/// and pad with a leading + trailing space so `" phrase "` substring checks are word-anchored.
fn normalize_for_match(msg: &str) -> String {
    let mut s = String::with_capacity(msg.len() + 2);
    s.push(' ');
    let mut prev_space = true;
    for c in msg.chars() {
        if c == '\'' {
            continue; // fold "don't" -> "dont", "can't" -> "cant"
        }
        let lc = c.to_ascii_lowercase();
        if lc.is_ascii_alphanumeric() {
            s.push(lc);
            prev_space = false;
        } else if !prev_space {
            s.push(' ');
            prev_space = true;
        }
    }
    if !s.ends_with(' ') {
        s.push(' ');
    }
    s
}

/// Whether `msg` is a deterministic approval of pending safeguard assumptions: it contains at
/// least one [`APPROVAL_PHRASES`] entry as a whole word/phrase AND no [`APPROVAL_NEGATIONS`]
/// token anywhere. The negation veto is what rejects the "I don't approve of waiting" false
/// positive; the whole-word anchoring is what keeps "disapprove" from reading as "approve".
pub(crate) fn is_approval_intent(msg: &str) -> bool {
    let h = normalize_for_match(msg);
    if APPROVAL_NEGATIONS.iter().any(|n| h.contains(&format!(" {n} "))) {
        return false;
    }
    APPROVAL_PHRASES.iter().any(|p| h.contains(&format!(" {p} ")))
}

/// Deterministic SDLC override-intent phrases (feature: sdlc-triage) — the SAME mechanism as
/// [`APPROVAL_PHRASES`], a new phrase set. When a project is on a LIGHT track (patch/enhancement)
/// and pre-authorize, one of these in a user message re-triages it to the full `project` ceremony.
const OVERRIDE_PHRASES: &[&str] = &[
    "full process", "make it a project", "make it a full project", "full project",
    "full ceremony", "treat it as a project", "do the full process", "full sdlc",
];

/// Whether `msg` is a deterministic SDLC override to the full project track (feature: sdlc-triage):
/// at least one [`OVERRIDE_PHRASES`] entry as a whole word/phrase AND no [`APPROVAL_NEGATIONS`]
/// token (so "don't make it a project" does NOT trigger) — mirroring [`is_approval_intent`].
pub(crate) fn is_override_intent(msg: &str) -> bool {
    let h = normalize_for_match(msg);
    if APPROVAL_NEGATIONS.iter().any(|n| h.contains(&format!(" {n} "))) {
        return false;
    }
    OVERRIDE_PHRASES.iter().any(|p| h.contains(&format!(" {p} ")))
}

/// Whether a project is still PRE-AUTHORIZE (feature: sdlc-triage): Drafting or Ready, i.e. the
/// board has not started grinding. SDLC override + enhancement->project escalation are only legal
/// here; once Running, the track is locked in.
fn is_pre_authorize(p: &Project) -> bool {
    matches!(p.phase, ProjectPhase::Drafting | ProjectPhase::Ready)
}

/// Re-triage a light-track project to the full `project` ceremony (feature: sdlc-triage) — the
/// user's explicit override, or an enhancement escalation via the same path. Sets the track, drops
/// the light-track drafting artifacts (board if one was built in Ready, the placeholder/partial
/// TRD+CRD, the gate + breakdown state, any pending assumptions), and — from Ready — regresses the
/// phase back to Drafting so the fuller ceremony re-runs. The captured change-brief (in the PRD
/// slot) is KEPT as carried context; the persona reply that follows (in `office_message`) now drafts
/// under the project contract. NEVER touches a Running/authorized project (guarded by the caller).
fn retriage_to_project(p: &mut Project, now_ms: u64, ctx: &mut Ctx) {
    let was_ready = matches!(p.phase, ProjectPhase::Ready);
    if let Ok(ph) = step_project(&p.phase, ProjectTransition::Retriage) {
        p.phase = ph;
    }
    p.track = "project".to_string();
    // A light track that reached Ready built a board; drop it so the project ceremony rebuilds it.
    if was_ready {
        p.epics.clear();
        p.stories.clear();
        p.tasks.clear();
    }
    // Drop light-track doc/gate placeholders; the project ceremony authors a real TRD+CRD + re-gates.
    // The change-brief in `prd_markdown` is deliberately KEPT as carried context.
    p.trd_markdown.clear();
    p.crd_markdown.clear();
    p.gate_cleared = false;
    p.gate_invoke_live_hint = false;
    p.pending_breakdown = None;
    p.pending_assumptions.clear();
    trace(p, now_ms, "sdlc", "re-triaged to project (user override)", ctx);
    queue_notice(
        p,
        now_ms,
        format!("office[{}]: reclassified as a full project — re-drafting under the full process.", p.id.0),
        ctx,
    );
    ctx.dirty = true;
}

/// Fire the intake TRIAGE classifier invoke (feature: sdlc-triage) on the `safeguard_role` — a
/// lightweight gate-family classification, NOT a doc-drafting invoke (never `drafter_model`). Sets
/// `triage_pending` so the persona reply's doc-capture is suppressed until the track is known.
fn start_triage(p: &mut Project, brief: &str, now_ms: u64, ctx: &mut Ctx) {
    let (system, prompt) = office::build_triage_prompt(p, brief);
    p.triage_pending = true;
    trace(p, now_ms, "sdlc", "triage — classifying the brief", ctx);
    emit_invoke(ctx, InvokePurpose::Triage, &p.config.safeguard_role, system, prompt);
}

/// The intake TRIAGE classifier returned (feature: sdlc-triage). Parse defensively (unparseable /
/// error -> `project`), store the track, trace + notice the verdict, and route: `patch` builds the
/// one-task board straight to Ready; `enhancement`/`project` just record the track (the persona now
/// drafts under the track-aware contract). Always clears `triage_pending` so capture can proceed.
fn handle_triage_result(p: &mut Project, outcome: Result<String, String>, now_ms: u64, ctx: &mut Ctx) {
    p.triage_pending = false;
    ctx.dirty = true;
    let verdict = match &outcome {
        Ok(text) => report::parse_triage(text),
        Err(e) => {
            trace(p, now_ms, "sdlc", format!("triage errored — defaulting to project: {}", trace_preview(e, 60)), ctx);
            report::TriageVerdict::project_default()
        }
    };
    let track = verdict.track.as_str();
    p.track = track.to_string();
    let rationale = if verdict.rationale.trim().is_empty() {
        "no rationale given".to_string()
    } else {
        verdict.rationale.clone()
    };
    trace(p, now_ms, "sdlc", format!("classified as {} — {}", track, trace_preview(&rationale, 80)), ctx);
    queue_notice(
        p,
        now_ms,
        format!("office[{}]: intake classified as {} — {}", p.id.0, track, trace_preview(&rationale, 160)),
        ctx,
    );
    if matches!(verdict.track, report::TriageTrack::Patch) {
        build_patch_board(p, now_ms, ctx);
    }
}

/// The intake brief text for a patch task (feature: sdlc-triage): the FIRST user turn in the
/// transcript — reliably the brief, since a patch board is built immediately after the first message
/// (before any folding). Empty when somehow absent.
fn intake_brief_text(p: &Project) -> String {
    p.office_transcript
        .iter()
        .find(|m| matches!(m.who, ChatAuthor::User))
        .map(|m| m.text.clone())
        .unwrap_or_default()
}

/// A short single-line title for a patch task, derived from the brief's first non-empty line.
fn patch_task_title(brief: &str) -> String {
    let first = brief
        .lines()
        .map(|l| l.trim().trim_start_matches(['#', '-', '*', ' ']))
        .find(|l| !l.is_empty())
        .unwrap_or("Patch");
    let t = trace_preview(first, 80);
    if t.is_empty() {
        "Patch".to_string()
    } else {
        t
    }
}

/// Build the PATCH-track board programmatically (feature: sdlc-triage): NO documents, NO breakdown
/// invoke — the brief text becomes a single Todo task and the project goes straight Drafting ->
/// Ready, awaiting authorize. Grind + merge review then run as normal; the final audit is skipped
/// (merge review is the gate — see [`maybe_complete_project`]). Guarded to Drafting so a late/racey
/// call can never rebuild a board.
fn build_patch_board(p: &mut Project, now_ms: u64, ctx: &mut Ctx) {
    if !matches!(p.phase, ProjectPhase::Drafting) {
        return;
    }
    let brief = intake_brief_text(p);
    let brief = if brief.trim().is_empty() {
        "Apply the requested change.".to_string()
    } else {
        brief
    };
    let proj = p.id.0.clone();
    let epic_id = crate::domain::EpicId(format!("{}/patch", proj));
    let story_id = crate::domain::StoryId(format!("{}/patch/change", proj));
    let task_id = TaskId(format!("{}/patch/change/apply", proj));
    let task = Task {
        id: task_id.clone(),
        title: patch_task_title(&brief),
        description: brief,
        acceptance: vec![
            "The change described in the task is fully implemented and working".to_string(),
            "The delivered tree stays clean (no trash or dead files; README preserved)".to_string(),
        ],
        blocked_by: Vec::new(),
        priority: 0,
        state: TaskState::Todo,
        bounces: 0,
        comments: Vec::new(),
        desk: None,
        last_report: None,
        last_review: None,
        history: Vec::new(),
        diff_stat: None,
        awaiting_merge: false,
        dispatch_after_ms: 0,
    };
    p.epics = vec![crate::domain::Epic {
        id: epic_id,
        title: "Patch".to_string(),
        intent: "A single-task patch".to_string(),
        stories: vec![story_id.clone()],
    }];
    p.stories = vec![crate::domain::Story {
        id: story_id,
        title: "Change".to_string(),
        intent: "The requested change".to_string(),
        tasks: vec![task_id],
    }];
    p.tasks = vec![task];
    if let Ok(ph) = step_project(&p.phase, ProjectTransition::AcceptBreakdown) {
        p.phase = ph;
    }
    trace(p, now_ms, "sdlc", "patch board built — 1 task, straight to Ready", ctx);
    queue_notice(
        p,
        now_ms,
        format!(
            "office[{}]: board is ready — 1 task (patch track). Authorize with a delivery path (workflow_authorize) to start the production line.",
            p.id.0
        ),
        ctx,
    );
    ctx.dirty = true;
}

/// Append the user turn and issue a persona invoke. If the assembled prompt would cross
/// the fold threshold, a summarize invoke is issued FIRST (6.2); the persona invoke is
/// re-issued from `invoke_result` once the fold lands.
///
/// Approval short-circuit: when the project is stopped on flagged assumptions and the incoming
/// message is a deterministic approval, the gate is closed for THIS project first
/// (`assumptions_approved`), `pending_assumptions` cleared, and a trace notice queued — so the
/// persona invoke that follows re-emits the doc and `gate_doc` fails open instead of re-stopping.
fn office_message(p: &mut Project, text: String, now_ms: u64, ctx: &mut Ctx) {
    // SDLC override (feature: sdlc-triage): a clear "make it a full project" intent on a LIGHT track
    // (patch/enhancement) that is still pre-authorize re-triages it to the full ceremony. Checked
    // before anything else, and naturally a no-op on a fresh project (track is still "project").
    if p.track != "project" && is_pre_authorize(p) && is_override_intent(&text) {
        retriage_to_project(p, now_ms, ctx);
    }

    if !p.pending_assumptions.is_empty() && is_approval_intent(&text) {
        p.assumptions_approved = true;
        p.pending_assumptions.clear();
        // The queued notice is the durable user-facing signal (outbox row + chat.prompt effect);
        // the trace ring records the same event on the machine diary.
        trace(p, now_ms, "gate", "approval detected — safeguard gate closed for this project", ctx);
        queue_notice(
            p,
            now_ms,
            format!("office[{}]: assumptions approved — gate closed for this project.", p.id.0),
            ctx,
        );
    }

    // Intake TRIAGE (feature: sdlc-triage): the FIRST message of a fresh, docless Drafting project
    // fires ONE additional lightweight classifier invoke ALONGSIDE the persona reply below. Guarded
    // so it fires exactly once per project and never on a mid-drafting continuation: an empty
    // transcript (this is the first user turn), still Drafting, no PRD/change-brief captured yet, and
    // none already in flight. Built from `text` before it is moved into the transcript.
    let first_message = p.office_transcript.is_empty();
    if first_message
        && matches!(p.phase, ProjectPhase::Drafting)
        && p.prd_markdown.trim().is_empty()
        && !p.triage_pending
    {
        start_triage(p, &text, now_ms, ctx);
    }

    // Trace BEFORE the move: the preview is the first ~80 chars, never the whole message.
    trace(p, now_ms, "office", format!("message received: {}", trace_preview(&text, 80)), ctx);
    p.office_transcript.push(ChatMsg {
        who: ChatAuthor::User,
        text,
    });
    ctx.dirty = true;

    if office::should_fold(p, "") {
        let (system, prompt) = office::build_fold(p);
        trace(p, now_ms, "invoke", "fold (summarize transcript)", ctx);
        emit_invoke(ctx, InvokePurpose::Fold, &p.config.office_role, system, prompt);
    } else {
        let (system, prompt) = office::build_invoke(p, "");
        trace(p, now_ms, "invoke", "persona reply", ctx);
        emit_draft_invoke(p, ctx, InvokePurpose::Persona, system, prompt);
    }
}

/// Issue the breakdown invoke for the current PRD (6.3.2).
fn request_breakdown(p: &mut Project, now_ms: u64, ctx: &mut Ctx) {
    let (system, prompt) = office::build_breakdown_prompt(p, None, false);
    trace(p, now_ms, "breakdown", "requested", ctx);
    emit_invoke(ctx, InvokePurpose::Breakdown, &p.config.office_role, system, prompt);
}

// ---------------------------------------------------------------------------
// Research + TRD pipeline (6.2b) — Drafting-only, deterministic, graceful-degrade
// ---------------------------------------------------------------------------

/// Whether `agent_id` is this project's live research binding (6.2b). Provisional (id 0)
/// bindings never match a real host event.
fn research_bound_to(p: &Project, agent_id: u64) -> bool {
    matches!(&p.research, Some(b) if b.ext_agent_id == agent_id && agent_id != PROVISIONAL)
}

/// Kick off the web-research spawn after a PRD is captured (6.2b). Two-phase like a worker
/// spawn: emit `SpawnResearch` and record a PROVISIONAL project-level binding so the reconcile
/// ceiling can see it; the driver runs the spawn and feeds back `ResearchSpawned` with the real
/// id (or `ResearchFailed`, which degrades to a PRD-only TRD).
fn start_research(p: &mut Project, now_ms: u64, ctx: &mut Ctx) {
    let prompt = prompts::research(p);
    ctx.fx.push(Effect::SpawnResearch { prompt });
    p.research = Some(AgentBinding {
        ext_agent_id: PROVISIONAL,
        session: p.bound_session.clone().unwrap_or_default(),
        spawned_at_ms: now_ms,
        kind: AgentKind::Researcher,
        // Project-level fixed staff: the office view keys the researcher desk off this
        // binding's PRESENCE, not a persona label, so no per-task persona applies.
        persona: String::new(),
    });
    trace(p, now_ms, "research", "spawned — analyzing the stack", ctx);
}

/// The driver recorded the real research agent id onto the provisional binding (6.2b).
fn on_research_spawned(p: &mut Project, agent_id: u64, spawned_at_ms: u64, ctx: &mut Ctx) {
    if let Some(b) = &mut p.research {
        b.ext_agent_id = agent_id;
        b.spawned_at_ms = spawned_at_ms;
        ctx.dirty = true;
    }
}

/// The researcher finished (6.2b / design-speedup item 2): parse the OFFICE-RESEARCH findings
/// (tolerant; a missing block falls back to the whole reply text), store the capped notes, clear
/// the binding, and try the TRD+CRD authoring join. Because research now runs IN PARALLEL with the
/// PRD gate, completion does not author the docs directly — it settles the research side of the
/// join and lets [`maybe_author_trdcrd`] author only once the PRD gate has ALSO cleared.
fn on_research_result(p: &mut Project, text: String, now_ms: u64, ctx: &mut Ctx) {
    let research_spawned_at_ms = p.research.as_ref().map(|b| b.spawned_at_ms);
    p.research_notes = office::extract_research(&text);
    p.research = None;
    trace(p, now_ms, "research", format!("done — {} bytes of notes", p.research_notes.len()), ctx);
    queue_notice(
        p,
        now_ms,
        format!("office[{}]: research done — drafting the TRD + clean-build requirements.", p.id.0),
        ctx,
    );
    maybe_author_trdcrd(p, now_ms, research_spawned_at_ms, ctx);
    ctx.dirty = true;
}

/// Research could not run or died — spawn failure, dead researcher, or runtime ceiling (6.2b).
/// Degrade gracefully: clear the binding, tell the user, and settle the research side of the
/// TRD+CRD join from the PRD alone. Never wedges Drafting.
fn research_degrade(p: &mut Project, reason: String, now_ms: u64, ctx: &mut Ctx) {
    let research_spawned_at_ms = p.research.as_ref().map(|b| b.spawned_at_ms);
    p.research = None;
    trace(p, now_ms, "research", format!("degraded: {}", trace_preview(&reason, 80)), ctx);
    queue_notice(
        p,
        now_ms,
        format!(
            "office[{}]: research skipped: {}; drafting the TRD + clean-build requirements from the PRD alone.",
            p.id.0, reason
        ),
        ctx,
    );
    maybe_author_trdcrd(p, now_ms, research_spawned_at_ms, ctx);
    ctx.dirty = true;
}

/// Whether a resumed Drafting project should have its researcher respawned (design-speedup item 6):
/// a captured PRD, no research notes yet, no researcher in flight, research not disabled by config,
/// and the TRD+CRD not yet authored. Derived purely from state — a hard interrupt clears the
/// research binding, so "was mid-research" is exactly this window.
fn resume_should_respawn_research(p: &Project) -> bool {
    matches!(p.phase, ProjectPhase::Drafting)
        && !p.prd_markdown.trim().is_empty()
        && p.research_notes.trim().is_empty()
        && p.research.is_none()
        && p.config.research_mode != "never"
        && p.trd_markdown.trim().is_empty()
        && p.crd_markdown.trim().is_empty()
}

/// Kick off research at PRD-capture time per `research_mode` (design-speedup item 2 + 4):
/// `"always"` -> spawn now (in PARALLEL with the PRD gate); `"never"` -> skip (traced); `"auto"` ->
/// DEFER the decision to the PRD gate's enumerate result, which asks the well-known boolean.
fn start_research_at_capture(p: &mut Project, now_ms: u64, ctx: &mut Ctx) {
    match p.config.research_mode.as_str() {
        "never" => trace(p, now_ms, "research", "research skipped (config)", ctx),
        "always" => start_research(p, now_ms, ctx),
        _ => {} // "auto": decided from the PRD gate's well-known answer
    }
}

/// In `research_mode == "auto"`, decide whether to run research from the PRD gate's `well-known:`
/// answer (design-speedup item 4). Only fires when research has not already been started or
/// completed (so an `"always"` spawn, or a completed run, is never disturbed). A missing/unparseable
/// answer defaults to running research.
fn research_decide_from_check(p: &mut Project, text: &str, now_ms: u64, ctx: &mut Ctx) {
    if p.config.research_mode != "auto" || p.research.is_some() || !p.research_notes.trim().is_empty() {
        return;
    }
    match office::parse_well_known(text) {
        Some(true) => trace(p, now_ms, "research", "research skipped — stack well-known", ctx),
        _ => start_research(p, now_ms, ctx),
    }
}

/// When there is no PRD gate result to read a well-known answer from (the gate was disabled/approved
/// or errored), `research_mode == "auto"` cannot ask — so it DEFAULTS to running research (item 4:
/// "if no or unparseable, run research"). No-op unless auto and research is still undecided.
fn research_decide_default(p: &mut Project, now_ms: u64, ctx: &mut Ctx) {
    if p.config.research_mode == "auto" && p.research.is_none() && p.research_notes.trim().is_empty() {
        start_research(p, now_ms, ctx);
    }
}

/// Kill the in-flight researcher (if any) and respawn it against the just-revised PRD (design-speedup
/// item 2): a gate auto-resolution that REVISES the PRD makes any research based on the old PRD
/// stale. No-op when no researcher is running (e.g. `research_mode == "auto"` before the decision, or
/// a completed/degraded run).
fn restart_research_if_running(p: &mut Project, now_ms: u64, ctx: &mut Ctx) {
    if let Some(b) = p.research.take() {
        if b.ext_agent_id != PROVISIONAL {
            ctx.fx.push(Effect::Kill { ext_agent_id: b.ext_agent_id });
        }
        trace(p, now_ms, "research", "research restarted — PRD revised", ctx);
        start_research(p, now_ms, ctx);
    }
}

/// The TRD+CRD authoring join (design-speedup items 2 + 3): author BOTH docs in ONE invoke, but
/// ONLY once the PRD gate has cleared AND research has SETTLED (done / degraded / skipped =
/// `research` is `None`). Called from both settle events (the PRD gate clearing, and research
/// completing/degrading/being skipped), so whichever finishes LAST triggers the authoring. Guarded
/// by "TRD+CRD not already captured" so it authors exactly once. `research_spawned_at_ms` is the
/// JUST-cleared research binding's spawn time (when the caller is a research settle event; `None`
/// when the caller is the gate clearing itself, where it is unused) — see
/// [`self_heal_stale_prd_gate`], which runs first and may presume the gate cleared for
/// pre-migration state.
fn maybe_author_trdcrd(p: &mut Project, now_ms: u64, research_spawned_at_ms: Option<u64>, ctx: &mut Ctx) {
    self_heal_stale_prd_gate(p, now_ms, research_spawned_at_ms, ctx);
    if !(p.gate_cleared && p.research.is_none()) {
        return;
    }
    // SDLC enhancement track (feature: sdlc-triage): SKIP the full TRD/CRD trio — go straight to a
    // small breakdown (the change-brief gate already cleared, standing in for the second gate too).
    if p.track == "enhancement" {
        maybe_start_enhancement_breakdown(p, now_ms, ctx);
    } else if p.trd_markdown.trim().is_empty() && p.crd_markdown.trim().is_empty() {
        start_trdcrd_invoke(p, now_ms, ctx);
    }
}

/// The ENHANCEMENT-track post-change-brief join (feature: sdlc-triage): once the change-brief gate
/// has cleared AND research settled, skip the TRD/CRD trio and start a SMALL breakdown directly. The
/// CRD is inherited if a prior one exists, else a minimal hygiene-only CRD is generated
/// PROGRAMMATICALLY (no invoke round) so the completion audit still has a checklist. Once-only:
/// guarded on no stashed breakdown, an empty board, and still Drafting (matching the project join's
/// "not already captured" idempotency).
fn maybe_start_enhancement_breakdown(p: &mut Project, now_ms: u64, ctx: &mut Ctx) {
    if p.pending_breakdown.is_some()
        || !p.tasks.is_empty()
        || !matches!(p.phase, ProjectPhase::Drafting)
    {
        return;
    }
    if p.crd_markdown.trim().is_empty() {
        p.crd_markdown = office::minimal_hygiene_crd();
        trace(p, now_ms, "sdlc", "enhancement: minimal hygiene CRD generated", ctx);
    } else {
        trace(p, now_ms, "sdlc", "enhancement: inheriting existing CRD", ctx);
    }
    trace(p, now_ms, "sdlc", "enhancement: skipping TRD/CRD trio — small breakdown", ctx);
    start_early_breakdown(p, now_ms, ctx);
}

/// Self-heal a PRD gate wedged by pre-migration state (review finding, design-speedup
/// `gate_cleared` field): a `state.json` persisted by a pre-6.2b-design-speedup build never had
/// `gate_cleared` at all (serde-default `false` on load) even though, under the OLD flow, research
/// only ever started AFTER the PRD gate had passed — so clearance was real but never persisted.
/// Reloaded into THIS build, no `AssumeCheckPrd` invoke will ever be (re-)fired for that PRD: the
/// stale researcher binding just settles via a dead-agent event or the reconcile runtime ceiling,
/// [`research_degrade`]/[`on_research_result`]/[`skip_research`] call the JOIN here, and —
/// unhealed — `gate_cleared` stays `false` forever, wedging Drafting silently while the degrade
/// notice claims drafting is proceeding.
///
/// Presumes the gate cleared when ALL of: the PRD exists, nothing is waiting on the user
/// (`pending_assumptions` empty — a live freeze could still resolve on its own, so never heal
/// under it), and no PRD gate invoke can plausibly still be in flight. That last part is decided
/// by TWO signals, either sufficient (an OR):
///
/// 1. **`Project.gate_invoke_live_hint`** (primary) — a `#[serde(skip)]` runtime-only flag: "a PRD
///    gate invoke was fired by THIS process and has not yet reached a terminal outcome". An
///    in-flight invoke can never survive a daemon restart or lease transfer (nothing persists it,
///    by design — ARCHITECTURE.md 6.2c: "the in-flight invoke chain IS the live signal, no disk
///    waiting-state"), so it deserializes to `false` unconditionally. A project reloaded from disk
///    — whether pre-migration (never had `gate_cleared` at all) or simply restarted mid-gate on
///    THIS schema — therefore has `false` here even if the in-memory state before the restart said
///    otherwise, which is exactly the "process boundary, not age" signal: it heals on the FIRST
///    settle after ANY reload, however young the respawned research binding is (fixes the
///    fast-upgrade-kill and already-killed-pre-upgrade-then-respawned-on-resume cases, which both
///    settle well under one `worker_max_runtime_ms`).
/// 2. **Research-age staleness** (belt) — reuses the SAME staleness signal `runtime_ceiling`
///    already applies to a hung sub-agent binding (`now_ms - spawned_at_ms > worker_max_runtime_ms`
///    on the research binding that just settled). Covers the case where the hint is (incorrectly or
///    not) `true` but so much time has passed that no single model call could still be outstanding —
///    in a live session research and the gate are kicked off in the SAME kernel step
///    (`start_research_at_capture` then `gate_doc`), and every `AssumeCheck*`/resolve/verify invoke
///    resolves far inside one ceiling window.
///
/// The native in-process race (`research_result_before_the_gate_clears_waits_for_the_join`) has the
/// hint `true` and a young binding, so NEITHER signal fires — the gate is correctly left to clear on
/// its own invoke result, unhealed.
fn self_heal_stale_prd_gate(p: &mut Project, now_ms: u64, research_spawned_at_ms: Option<u64>, ctx: &mut Ctx) {
    if p.gate_cleared || p.prd_markdown.trim().is_empty() || !p.pending_assumptions.is_empty() {
        return;
    }
    let age_proves_stale = matches!(
        research_spawned_at_ms,
        Some(spawned_at_ms) if now_ms.saturating_sub(spawned_at_ms) > p.config.worker_max_runtime_ms
    );
    if p.gate_invoke_live_hint && !age_proves_stale {
        return; // fired by THIS process and not even runtime-ceiling-old — genuinely may still land
    }
    p.gate_cleared = true;
    p.gate_invoke_live_hint = false;
    trace(p, now_ms, "gate", "PRD gate presumed cleared (migrated state)", ctx);
}

/// Issue the COMBINED TRD+CRD authoring invoke (design-speedup item 3): one invoke authors BOTH
/// docs. Uses `drafter_model` when set (a doc-drafting invoke, item 4).
fn start_trdcrd_invoke(p: &mut Project, now_ms: u64, ctx: &mut Ctx) {
    let (system, prompt) = office::build_trdcrd_prompt(p);
    trace(p, now_ms, "invoke", "TRD+CRD authoring", ctx);
    emit_draft_invoke(p, ctx, InvokePurpose::TrdCrd, system, prompt);
}

/// The combined TRD+CRD invoke returned (design-speedup item 3 + 8). Capture BOTH fences (```trd
/// and ```crd); a capture-miss nudge fires (shared budget with the PRD nudge) if EITHER is missing,
/// so a model that drops one fence gets one narrower re-ask. Once at least one doc is captured, the
/// early breakdown will start when the gate finalizes the TRD, and the single combined TRD+CRD gate
/// runs over both docs. Any `Err` (after the driver's one retry) proceeds to the breakdown from
/// whatever docs exist so Drafting never wedges.
fn handle_trdcrd_result(p: &mut Project, outcome: Result<String, String>, now_ms: u64, ctx: &mut Ctx) {
    let text = match outcome {
        Ok(t) => t,
        Err(e) => {
            queue_notice(
                p,
                now_ms,
                format!(
                    "office[{}]: TRD+CRD call failed: {}; requesting the breakdown from the PRD.",
                    p.id.0, e
                ),
                ctx,
            );
            start_early_breakdown(p, now_ms, ctx);
            run_gate_cleared(p, Deferred::Breakdown, now_ms, ctx);
            ctx.dirty = true;
            return;
        }
    };
    let (trd, crd) = office::extract_trd_crd(&text);

    // Capture-miss nudge (shared budget): a long reply missing EITHER fence gets one narrower re-ask.
    if (trd.is_none() || crd.is_none())
        && text.len() > PRD_NUDGE_MIN_REPLY_BYTES
        && p.capture_nudge_count < MAX_CAPTURE_NUDGES
    {
        p.capture_nudge_count += 1;
        trace(p, now_ms, "nudge", format!("TRD+CRD capture-miss nudge #{}", p.capture_nudge_count), ctx);
        let (mut system, prompt) = office::build_trdcrd_prompt(p);
        system.push_str(TRDCRD_NUDGE_INSTRUCTION);
        emit_draft_invoke(p, ctx, InvokePurpose::TrdCrd, system, prompt);
        ctx.dirty = true;
        return;
    }

    // Fresh doc-set capture: reset the gate + discard any stale early breakdown (item 8 redo).
    reset_trdcrd_capture(p, now_ms, ctx);
    p.capture_nudge_count = 0;
    if let Some(t) = trd {
        p.trd_markdown = t;
    }
    if let Some(c) = crd {
        p.crd_markdown = c;
    }
    trace(
        p,
        now_ms,
        "capture",
        format!("TRD+CRD drafted (trd {}B, crd {}B)", p.trd_markdown.len(), p.crd_markdown.len()),
        ctx,
    );
    queue_notice(
        p,
        now_ms,
        format!("office[{}]: TRD + clean-build requirements drafted (panel) — checking assumptions before the breakdown.", p.id.0),
        ctx,
    );
    gate_doc(p, Deferred::Breakdown, now_ms, ctx);
    ctx.dirty = true;
}

/// Reset the per-doc-set gate + early-breakdown state on a fresh TRD+CRD capture (design-speedup
/// item 8 redo): a revised TRD invalidates any stashed early breakdown, so discard it and re-open
/// the gate. Traced when a stash was actually discarded.
fn reset_trdcrd_capture(p: &mut Project, now_ms: u64, ctx: &mut Ctx) {
    p.gate_cleared = false;
    if p.pending_breakdown.take().is_some() {
        trace(p, now_ms, "breakdown", "breakdown redone — TRD revised", ctx);
    }
}

// ---------------------------------------------------------------------------
// Safeguard one-shot gate + parallel joins (design-speedup items 2/3/5/8)
// ---------------------------------------------------------------------------

/// What a captured drafting doc-set's safeguard gate advances to once it clears. There are only two
/// doc-sets now (PRD, then combined TRD+CRD), and each is a JOIN — the gate clearing is only one of
/// two conditions. `PostPrd` = the PRD gate cleared; join with research to author TRD+CRD. `Breakdown`
/// = the TRD+CRD gate cleared; join with the early breakdown to build the board. The deferred is a
/// pure function of which docs are non-empty (`newest_gated_doc`), so it never needs persisting.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Deferred {
    PostPrd,
    Breakdown,
}

/// A gate cleared: record it on the shared `gate_cleared` flag (one flag serves both stages because
/// the PRD stage strictly precedes the TRD+CRD stage and each fresh capture resets it), drop any
/// pending assumptions, and run the stage's JOIN — which fires the next pipeline step only once BOTH
/// join conditions hold (the parallel research / early breakdown may still be in flight). This is
/// the single chokepoint every PRD-stage gate outcome funnels through (clean, fail-open on `Err`,
/// approved self-resolve, no-usable-items, resolve-failed, verify errored/clean/disclosed,
/// `workflow_approve`) — so it doubles as the terminal-arm clear site for
/// `gate_invoke_live_hint` (the freeze arm, which does NOT reach here, clears it in
/// `freeze_critical`).
fn run_gate_cleared(p: &mut Project, deferred: Deferred, now_ms: u64, ctx: &mut Ctx) {
    if matches!(deferred, Deferred::PostPrd) {
        p.gate_invoke_live_hint = false;
    }
    p.gate_cleared = true;
    p.pending_assumptions.clear();
    match deferred {
        // `gate_cleared` was just set `true` above, so `maybe_author_trdcrd`'s self-heal check is
        // already a no-op here — `research_spawned_at_ms` is unused.
        Deferred::PostPrd => maybe_author_trdcrd(p, now_ms, None, ctx),
        Deferred::Breakdown => maybe_apply_breakdown(p, now_ms, ctx),
    }
}

/// The early-breakdown JOIN (design-speedup item 8): apply the stashed breakdown and move Drafting
/// -> Ready, but ONLY once the TRD+CRD gate has cleared AND the early breakdown has landed
/// (`pending_breakdown` stashed). Called from both settle events, so whichever finishes LAST builds
/// the board — so by the time the user authorizes, the workers can start immediately.
fn maybe_apply_breakdown(p: &mut Project, now_ms: u64, ctx: &mut Ctx) {
    if !p.gate_cleared {
        return;
    }
    let text = match p.pending_breakdown.take() {
        Some(t) => t,
        None => return, // breakdown still in flight (or failed/surfaced) — wait
    };
    match office::parse_breakdown(&text) {
        // Apply DIRECTLY (never via apply_or_stash_breakdown, which would re-stash + re-enter here
        // in Drafting — an infinite loop): the stash is being consumed, so land it now.
        Ok(breakdown) => land_breakdown(p, breakdown, now_ms, ctx),
        Err(_) => {
            // Defensive only: a stash that re-parses invalid should be impossible (it validated on
            // the way in). Fall back to an inline breakdown rather than wedging.
            trace(p, now_ms, "breakdown", "stashed breakdown invalid — re-running inline", ctx);
            request_breakdown(p, now_ms, ctx);
        }
    }
}

/// Kick off the epic/story/task breakdown EARLY (design-speedup item 8): as soon as the TRD is
/// finalized by the gate, in parallel with the gate's verify pass. Its result is stashed
/// (`pending_breakdown`) rather than applied, and the JOIN (`maybe_apply_breakdown`) builds the
/// board once the gate clears. Structured-JSON invoke on the office role — NOT a doc-drafting invoke,
/// so it does NOT take `drafter_model`.
fn start_early_breakdown(p: &mut Project, now_ms: u64, ctx: &mut Ctx) {
    let (system, prompt) = office::build_breakdown_prompt(p, None, false);
    trace(p, now_ms, "breakdown", "started early (parallel with the gate verify)", ctx);
    emit_invoke(ctx, InvokePurpose::Breakdown, &p.config.office_role, system, prompt);
}

/// The Breakdown-stage finalize side-effect: when the TRD+CRD gate is about to CLEAR without going
/// through the verify path (a clean enumerate, a skipped gate, an approval, an errored check, or a
/// failed resolve), kick off the early breakdown now so the JOIN has a plan to apply. The verify
/// path finalizes inside [`emit_verify`] instead, so this and that are mutually exclusive per run.
fn finalize_trdcrd_if_needed(p: &mut Project, deferred: Deferred, now_ms: u64, ctx: &mut Ctx) {
    if matches!(deferred, Deferred::Breakdown) {
        start_early_breakdown(p, now_ms, ctx);
    }
}

/// User-driven skip-research (design-speedup item 7, `workflow_skip`). If the researcher is in
/// flight, kill it, mark research skipped (empty notes are fine), and advance the TRD+CRD authoring
/// join. If nothing is running, a friendly notice naming the project's phase (the pipeline is not
/// waiting on research, so there is nothing to skip).
fn skip_research(p: &mut Project, now_ms: u64, ctx: &mut Ctx) {
    if let Some(b) = p.research.take() {
        let research_spawned_at_ms = b.spawned_at_ms;
        if b.ext_agent_id != PROVISIONAL {
            ctx.fx.push(Effect::Kill { ext_agent_id: b.ext_agent_id });
        }
        trace(p, now_ms, "research", "research skipped by user", ctx);
        queue_notice(
            p,
            now_ms,
            format!(
                "office[{}]: research skipped by user — drafting the TRD + clean-build requirements.",
                p.id.0
            ),
            ctx,
        );
        maybe_author_trdcrd(p, now_ms, Some(research_spawned_at_ms), ctx);
        ctx.dirty = true;
    } else {
        queue_notice(
            p,
            now_ms,
            format!(
                "office[{}]: no research is running to skip (project is {}).",
                p.id.0,
                phase_label(&p.phase)
            ),
            ctx,
        );
        ctx.dirty = true;
    }
}

/// Gate a captured drafting doc-set through the ONE-SHOT safeguard gate (design-speedup item 5 +
/// amendment A). Fails OPEN when disabled (`assumption_check == false`) or already approved for this
/// project (`assumptions_approved`) — same shape as before, and the approval one is what breaks the
/// audit re-emit -> re-gate -> stop loop. Otherwise it emits the ENUMERATE pass on `safeguard_role`:
/// in `assumption_mode == "auto"` the pass ALSO resolves the non-critical items inline (compressed
/// gate), and on the PRD stage with `research_mode == "auto"` it also answers the well-known boolean.
/// The fixed gate parameters for a doc-set stage: the enumerate purpose, human label, and fence
/// tags. Purely a function of `deferred`, so callers need only supply the stage.
fn gate_params(deferred: Deferred) -> (InvokePurpose, &'static str, &'static [&'static str]) {
    match deferred {
        Deferred::PostPrd => (InvokePurpose::AssumeCheckPrd, "PRD", PRD_TAGS),
        Deferred::Breakdown => (InvokePurpose::AssumeCheckTrdCrd, "TRD+CRD", TRDCRD_TAGS),
    }
}

fn gate_doc(p: &mut Project, deferred: Deferred, now_ms: u64, ctx: &mut Ctx) {
    let (purpose, label, tags) = gate_params(deferred);
    if p.assumptions_approved || !p.config.assumption_check {
        let why = if p.assumptions_approved { "already approved" } else { "gate off" };
        p.pending_assumptions.clear();
        trace(p, now_ms, "gate", format!("{label} gate skipped ({why})"), ctx);
        // Fail-open still owes the stage's finalize side-effects: the PRD stage decides research
        // (auto defaults to running, having no well-known answer to read); the TRD+CRD stage starts
        // the early breakdown.
        if matches!(deferred, Deferred::PostPrd) {
            research_decide_default(p, now_ms, ctx);
        }
        finalize_trdcrd_if_needed(p, deferred, now_ms, ctx);
        run_gate_cleared(p, deferred, now_ms, ctx);
        return;
    }
    let resolve_inline = p.config.assumption_mode == "auto";
    let ask_wellknown = matches!(deferred, Deferred::PostPrd) && p.config.research_mode == "auto";
    let body = gate_body(p, deferred);
    let (system, prompt) =
        office::build_assume_check_prompt(p, label, &body, tags, resolve_inline, ask_wellknown);
    trace(p, now_ms, "gate", format!("checking {label} for assumptions"), ctx);
    if matches!(deferred, Deferred::PostPrd) {
        p.gate_invoke_live_hint = true;
    }
    emit_invoke(ctx, purpose, &p.config.safeguard_role, system, prompt);
}

/// The newest non-empty drafting doc-set + its gate parameters (human label, deferred join, fence
/// tags). The pipeline authors PRD then the combined TRD+CRD strictly in order, so the LAST gate
/// always ran on the newest doc-set: the TRD+CRD set (either doc present) wins over the PRD. Recovers
/// exactly what `pending_assumptions` / an in-flight resolve/verify belongs to with no extra
/// persisted state. `None` only before any doc exists. (The enumerate purpose + body are re-derived
/// on demand from `deferred` via `gate_params`/`gate_body`, so they are not returned here.)
fn newest_gated_doc(p: &Project) -> Option<(&'static str, Deferred, &'static [&'static str])> {
    if !p.trd_markdown.trim().is_empty() || !p.crd_markdown.trim().is_empty() {
        Some(("TRD+CRD", Deferred::Breakdown, TRDCRD_TAGS))
    } else if !p.prd_markdown.trim().is_empty() {
        Some(("PRD", Deferred::PostPrd, PRD_TAGS))
    } else {
        None
    }
}

/// The current body of the doc-set a gate is operating on, for the resolve/verify prompts.
fn gate_body(p: &Project, deferred: Deferred) -> String {
    match deferred {
        Deferred::PostPrd => p.prd_markdown.clone(),
        Deferred::Breakdown => office::trdcrd_body(p),
    }
}

/// Re-run the safeguard gate on the newest captured drafting doc-set (feature 1). Called from the
/// Persona arm when a fenceless reply arrives while `pending_assumptions` is set: the transcript now
/// carries the user's fresh approval / answers / delegation, so the SAME gate is re-emitted to
/// re-judge it. No-op if there is somehow no doc to gate.
fn recheck_pending_assumptions(p: &mut Project, now_ms: u64, ctx: &mut Ctx) {
    if let Some((_, deferred, _)) = newest_gated_doc(p) {
        gate_doc(p, deferred, now_ms, ctx);
    }
}

/// Explicit human approval of the pending safeguard assumptions (feature 2, `workflow_approve`).
/// Human approval OUTRANKS the checker: record the approval as a User turn, set the sticky
/// `assumptions_approved`, clear `pending_assumptions` DIRECTLY, and clear the gate for the newest
/// doc-set — no re-invoke. With nothing pending it is a no-op beyond a notice.
fn approve_assumptions(p: &mut Project, now_ms: u64, ctx: &mut Ctx) {
    p.office_transcript.push(ChatMsg {
        who: ChatAuthor::User,
        text: "Approved: proceed with your proposed choices (delegated).".to_string(),
    });
    ctx.dirty = true;
    if p.pending_assumptions.is_empty() {
        queue_notice(p, now_ms, format!("office[{}]: nothing awaiting approval.", p.id.0), ctx);
        return;
    }
    p.assumptions_approved = true;
    p.pending_assumptions.clear();
    trace(p, now_ms, "gate", "workflow_approve — safeguard gate closed for this project", ctx);
    queue_notice(
        p,
        now_ms,
        format!("office[{}]: assumptions approved by user — resuming.", p.id.0),
        ctx,
    );
    if let Some((_, deferred, _)) = newest_gated_doc(p) {
        // The gate ran (it froze), so research was already decided; only the stage finalize is owed.
        finalize_trdcrd_if_needed(p, deferred, now_ms, ctx);
        run_gate_cleared(p, deferred, now_ms, ctx);
    }
}

/// The ENUMERATE pass returned (design-speedup one-shot gate). Order of work: (1) apply any inline
/// revision the compressed auto-mode gate returned; (2) settle research for the PRD stage (well-known
/// decision, or restart on a PRD revision); (3) route the verdict — clean/approved -> clear;
/// [critical] -> freeze; all-[auto] auto-mode -> the doc is already revised, run VERIFY; all-[auto]
/// ask-mode -> emit the batch RESOLVE. An `Err` fails open. Never loops: the resolution is bounded to
/// one pass and the verify may only clear or disclose.
fn handle_assume_check_result(
    p: &mut Project,
    deferred: Deferred,
    doc_label: &str,
    tags: &'static [&'static str],
    outcome: Result<String, String>,
    now_ms: u64,
    ctx: &mut Ctx,
) {
    ctx.dirty = true;
    let text = match outcome {
        Err(e) => {
            trace(p, now_ms, "gate", format!("{doc_label} check errored — failing open"), ctx);
            queue_notice(
                p,
                now_ms,
                format!("office[{}]: assumption check skipped: {}; continuing.", p.id.0, e),
                ctx,
            );
            if matches!(deferred, Deferred::PostPrd) {
                research_decide_default(p, now_ms, ctx);
            }
            finalize_trdcrd_if_needed(p, deferred, now_ms, ctx);
            run_gate_cleared(p, deferred, now_ms, ctx);
            return;
        }
        Ok(t) => t,
    };

    let check = report::parse_assume_check(&text);
    let verdict = check.as_ref().map(|c| c.verdict).unwrap_or(report::AssumeVerdict::Clean);
    let items: Vec<String> = check.map(|c| c.items).unwrap_or_default();
    let auto_mode = p.config.assumption_mode == "auto";

    // SDLC escalation (feature: sdlc-triage): an ENHANCEMENT whose change-brief gate (PostPrd)
    // surfaces MORE than N material assumptions is wider than a change -> escalate to the full
    // project track. Flip the track + trace here; the gate then finishes normally (resolve/freeze/
    // verify on the change-brief), and on clear the PostPrd join authors TRD+CRD (project branch)
    // instead of the small enhancement breakdown. Pre-authorize by construction (the gate runs in
    // Drafting).
    if matches!(deferred, Deferred::PostPrd)
        && p.track == "enhancement"
        && verdict == report::AssumeVerdict::Assumptions
        && items.len() > ENHANCEMENT_ASSUMPTION_ESCALATION_MAX
    {
        trace(p, now_ms, "sdlc", "escalating enhancement → project", ctx);
        queue_notice(
            p,
            now_ms,
            format!(
                "office[{}]: {} material assumptions on the change — wider than an enhancement; reclassified as a full project.",
                p.id.0,
                items.len()
            ),
            ctx,
        );
        p.track = "project".to_string();
    }

    // (1) Inline revision — only the compressed auto-mode gate revises, and only on an assumptions
    // verdict.
    let revised = auto_mode
        && verdict == report::AssumeVerdict::Assumptions
        && apply_revised_docs(p, tags, &text);
    if revised {
        trace(p, now_ms, "gate", format!("{doc_label}: resolved assumption(s) inline"), ctx);
    }

    // (2) Research settle (PRD stage only). Restart FIRST (a no-op unless a researcher is already
    // running, i.e. always-mode) so a PRD revised inline restarts against the new PRD; THEN decide
    // from the well-known answer (auto-mode), which starts a fresh researcher against the
    // already-revised PRD — so the two never double-spawn.
    if matches!(deferred, Deferred::PostPrd) {
        if revised {
            restart_research_if_running(p, now_ms, ctx);
        }
        research_decide_from_check(p, &text, now_ms, ctx);
    }

    // (3) Verdict routing.
    if verdict == report::AssumeVerdict::Clean {
        trace(p, now_ms, "gate", format!("{doc_label} clean — proceeding"), ctx);
        if !p.pending_assumptions.is_empty() {
            queue_notice(p, now_ms, format!("office[{}]: assumptions resolved — resuming.", p.id.0), ctx);
        }
        finalize_trdcrd_if_needed(p, deferred, now_ms, ctx);
        run_gate_cleared(p, deferred, now_ms, ctx);
        return;
    }

    // assumptions verdict — race belt first.
    if p.assumptions_approved {
        let n = items.len();
        record_self_resolved(p, &items);
        trace(p, now_ms, "gate", format!("{doc_label} self-resolved {n} assumption(s) (approved)"), ctx);
        queue_notice(
            p,
            now_ms,
            format!(
                "office[{}]: no-assume (approved): self-resolved {} assumption{}, proceeding.",
                p.id.0,
                n,
                if n == 1 { "" } else { "s" }
            ),
            ctx,
        );
        finalize_trdcrd_if_needed(p, deferred, now_ms, ctx);
        run_gate_cleared(p, deferred, now_ms, ctx);
        return;
    }

    let (critical, auto) = partition_assumptions(&items);

    if !critical.is_empty() {
        // BOTH modes stop on critical items (auto mode already resolved the [auto] ones inline; ask
        // mode surfaces critical BEFORE any rewrite). No finalize — the pipeline is frozen. This is
        // the OTHER PRD-stage terminal arm (besides `run_gate_cleared`): nothing is in flight while
        // frozen on the user, so clear the hint here too.
        if matches!(deferred, Deferred::PostPrd) {
            p.gate_invoke_live_hint = false;
        }
        freeze_critical(p, doc_label, critical, now_ms, ctx);
        return;
    }

    if auto.is_empty() {
        // A dirty verdict with no usable items -> nothing to resolve; clear + proceed.
        finalize_trdcrd_if_needed(p, deferred, now_ms, ctx);
        run_gate_cleared(p, deferred, now_ms, ctx);
        return;
    }

    if auto_mode {
        // The doc-set was already revised inline; go straight to the single VERIFY pass.
        emit_verify(p, deferred, doc_label, now_ms, ctx);
    } else {
        // 'ask' mode: batch-resolve the non-critical remainder, then verify.
        emit_resolve(p, deferred, doc_label, auto, tags, now_ms, ctx);
    }
}

/// Emit the batch RESOLVE invoke ('ask' mode, one-shot gate). The office decides every non-critical
/// item itself and re-emits the revised doc(s). Doc-drafting/revision invoke, so it takes
/// `drafter_model` (item 4).
fn emit_resolve(
    p: &mut Project,
    deferred: Deferred,
    doc_label: &str,
    auto: Vec<String>,
    tags: &'static [&'static str],
    now_ms: u64,
    ctx: &mut Ctx,
) {
    let n = auto.len();
    p.pending_assumptions.clear();
    trace(p, now_ms, "gate", format!("{doc_label} resolving {n} auto assumption(s)"), ctx);
    queue_notice(
        p,
        now_ms,
        format!(
            "office[{}]: {} has {} non-critical assumption{} — resolving {} autonomously.",
            p.id.0,
            doc_label,
            n,
            if n == 1 { "" } else { "s" },
            if n == 1 { "it" } else { "them" }
        ),
        ctx,
    );
    let body = gate_body(p, deferred);
    let (system, prompt) = office::build_assume_resolve_prompt(p, doc_label, &body, &auto, tags);
    if matches!(deferred, Deferred::PostPrd) {
        p.gate_invoke_live_hint = true;
    }
    emit_draft_invoke(p, ctx, InvokePurpose::AssumeResolve, system, prompt);
}

/// Emit the FINAL VERIFY pass (one-shot gate, item 5c). For the TRD+CRD stage this is also where the
/// early breakdown starts (parallel with the verify), so the finalize happens exactly once. Runs on
/// the `safeguard_role`.
fn emit_verify(p: &mut Project, deferred: Deferred, doc_label: &str, now_ms: u64, ctx: &mut Ctx) {
    if matches!(deferred, Deferred::Breakdown) {
        start_early_breakdown(p, now_ms, ctx);
    }
    let body = gate_body(p, deferred);
    let (system, prompt) = office::build_assume_verify_prompt(p, doc_label, &body);
    trace(p, now_ms, "gate", format!("verifying {doc_label}"), ctx);
    if matches!(deferred, Deferred::PostPrd) {
        p.gate_invoke_live_hint = true;
    }
    emit_invoke(ctx, InvokePurpose::AssumeVerify, &p.config.safeguard_role, system, prompt);
}

/// The batch RESOLVE invoke returned ('ask' mode). `Ok` with a revised fence -> update the doc(s),
/// restart research on a PRD revision, and run the single VERIFY pass. A missing fence or any `Err`
/// PROCEEDS (never wedges). The doc-set is recovered from pure state (`newest_gated_doc`).
fn handle_assume_resolve_result(p: &mut Project, outcome: Result<String, String>, now_ms: u64, ctx: &mut Ctx) {
    ctx.dirty = true;
    let (label, deferred, tags) = match newest_gated_doc(p) {
        Some(x) => x,
        None => return,
    };
    match outcome {
        Ok(text) => {
            if apply_revised_docs(p, tags, &text) {
                trace(p, now_ms, "capture", format!("{label} revised by auto-resolve"), ctx);
                if matches!(deferred, Deferred::PostPrd) {
                    restart_research_if_running(p, now_ms, ctx);
                }
                emit_verify(p, deferred, label, now_ms, ctx);
            } else {
                proceed_after_failed_resolve(p, deferred, "the resolver returned no revised document", now_ms, ctx);
            }
        }
        Err(e) => proceed_after_failed_resolve(p, deferred, &e, now_ms, ctx),
    }
}

/// The FINAL VERIFY pass returned (one-shot gate, item 5c). It may only CLEAR or DISCLOSE: any items
/// it flags are recorded as disclosed (`self_resolved_assumptions`) and the gate clears anyway — it
/// NEVER triggers another resolve round. An `Err` proceeds. The early breakdown was already emitted
/// when the verify was requested, so this only clears the gate.
fn handle_assume_verify_result(p: &mut Project, outcome: Result<String, String>, now_ms: u64, ctx: &mut Ctx) {
    ctx.dirty = true;
    let (label, deferred, _tags) = match newest_gated_doc(p) {
        Some(x) => x,
        None => return,
    };
    match outcome {
        Err(e) => {
            trace(p, now_ms, "gate", format!("{label} verify errored — proceeding"), ctx);
            queue_notice(p, now_ms, format!("office[{}]: verification skipped: {}; proceeding.", p.id.0, e), ctx);
            run_gate_cleared(p, deferred, now_ms, ctx);
        }
        Ok(text) => match report::parse_assume_check(&text) {
            Some(check)
                if check.verdict == report::AssumeVerdict::Assumptions && !check.items.is_empty() =>
            {
                let n = check.items.len();
                record_self_resolved(p, &check.items);
                trace(p, now_ms, "gate", format!("verified — {n} new item(s) disclosed, not re-looped"), ctx);
                queue_notice(
                    p,
                    now_ms,
                    format!(
                        "office[{}]: verified — {} assumption{} disclosed, proceeding.",
                        p.id.0,
                        n,
                        if n == 1 { "" } else { "s" }
                    ),
                    ctx,
                );
                run_gate_cleared(p, deferred, now_ms, ctx);
            }
            _ => {
                trace(p, now_ms, "gate", format!("{label} verified — clean"), ctx);
                run_gate_cleared(p, deferred, now_ms, ctx);
            }
        },
    }
}

/// Auto-resolution could not finish (invoke `Err` or a missing fence): disclose, run the stage
/// finalize (early breakdown for TRD+CRD), and clear the gate. Never wedges.
fn proceed_after_failed_resolve(p: &mut Project, deferred: Deferred, reason: &str, now_ms: u64, ctx: &mut Ctx) {
    trace(p, now_ms, "gate", format!("auto-resolve failed: {}", trace_preview(reason, 60)), ctx);
    queue_notice(
        p,
        now_ms,
        format!(
            "office[{}]: auto-resolution could not finish ({}); proceeding anyway — ultra-automatic mode never stalls.",
            p.id.0, reason
        ),
        ctx,
    );
    finalize_trdcrd_if_needed(p, deferred, now_ms, ctx);
    run_gate_cleared(p, deferred, now_ms, ctx);
}

/// Partition flagged items into (critical, auto), stripping the `[critical]`/`[auto]` tags. Bare
/// tags with no item text are dropped.
fn partition_assumptions(items: &[String]) -> (Vec<String>, Vec<String>) {
    let mut critical = Vec::new();
    let mut auto = Vec::new();
    for item in items {
        let c = report::classify_assumption(item);
        if c.text.is_empty() {
            continue;
        }
        if c.critical {
            critical.push(c.text);
        } else {
            auto.push(c.text);
        }
    }
    (critical, auto)
}

/// Apply the gate's revised fenced document(s) to the project (design-speedup gate revision). For
/// each tag in the doc-set, capture its fence and overwrite the matching doc if it changed. Returns
/// whether anything actually changed (drives the research restart + trace).
fn apply_revised_docs(p: &mut Project, tags: &[&str], text: &str) -> bool {
    let mut changed = false;
    // The combined TRD+CRD set needs the two-doc splitter so the first fence does not swallow the
    // second (extract_fenced is greedy by design); the PRD set is a single fence.
    if tags == TRDCRD_TAGS {
        let (trd, crd) = office::extract_trd_crd(text);
        if let Some(d) = trd {
            if p.trd_markdown != d {
                p.trd_markdown = d;
                changed = true;
            }
        }
        if let Some(d) = crd {
            if p.crd_markdown != d {
                p.crd_markdown = d;
                changed = true;
            }
        }
        return changed;
    }
    for tag in tags {
        if let Some(doc) = office::extract_fenced(text, tag) {
            match *tag {
                "prd" if p.prd_markdown != doc => {
                    p.prd_markdown = doc;
                    changed = true;
                }
                "trd" if p.trd_markdown != doc => {
                    p.trd_markdown = doc;
                    changed = true;
                }
                "crd" if p.crd_markdown != doc => {
                    p.crd_markdown = doc;
                    changed = true;
                }
                _ => {}
            }
        }
    }
    changed
}

/// Append safeguard-flagged assumptions to the project's disclosed-assumptions audit trail (tags
/// stripped), capped to the most recent [`SELF_RESOLVED_CAP`] entries so a long drafting session can
/// never balloon the state file. Shared by the approval race-belt and the verify-disclose path.
fn record_self_resolved(p: &mut Project, items: &[String]) {
    const SELF_RESOLVED_CAP: usize = 100;
    for item in items {
        let t = report::classify_assumption(item).text;
        if !t.is_empty() {
            p.self_resolved_assumptions.push(t);
        }
    }
    let len = p.self_resolved_assumptions.len();
    if len > SELF_RESOLVED_CAP {
        p.self_resolved_assumptions.drain(0..len - SELF_RESOLVED_CAP);
    }
}

/// A critical freeze: store ONLY the critical items as pending and notice the user that these
/// specific decisions need a human before anything happens. Both modes route here for critical items
/// (`ask` still stops on critical; `auto` freezes only the critical ones).
fn freeze_critical(p: &mut Project, doc_label: &str, critical: Vec<String>, now_ms: u64, ctx: &mut Ctx) {
    let preview = clip_assumptions(&critical);
    let n = critical.len();
    queue_notice(
        p,
        now_ms,
        format!(
            "office[{}]: {} critical assumption{} need you on the {}: {} — approve them, answer in chat, or say 'you decide' before I proceed.",
            p.id.0,
            n,
            if n == 1 { "" } else { "s" },
            doc_label,
            preview
        ),
        ctx,
    );
    p.pending_assumptions = critical;
    trace(p, now_ms, "gate", format!("{doc_label} STOPPED (critical) — {n} assumption(s) flagged"), ctx);
}

/// Clip the assumption list to a short, single-line preview for a chat notice (the full list is
/// on the panel via `pending_assumptions`).
fn clip_assumptions(items: &[String]) -> String {
    const MAX_ITEMS: usize = 3;
    const MAX_LEN: usize = 400;
    let mut preview: Vec<String> = items.iter().take(MAX_ITEMS).cloned().collect();
    if items.len() > MAX_ITEMS {
        preview.push(format!("(+{} more)", items.len() - MAX_ITEMS));
    }
    let joined = preview.join("; ");
    if joined.len() <= MAX_LEN {
        return joined;
    }
    let mut cut = MAX_LEN;
    while !joined.is_char_boundary(cut) {
        cut -= 1;
    }
    format!("{}…", &joined[..cut])
}

/// The hard authorization gate (6.3.3). The driver has already validated + created the
/// path; `delivery_valid` is its verdict, and `office::authorize` re-checks the shape
/// before transitioning `Ready -> Running`. `worktree` is the driver's git-repo setup verdict
/// (item 1): `Ok` turns worktree desks on for the project; `Err(reason)` records the fallback to
/// legacy copy-desks with the reason traced.
fn authorize(
    p: &mut Project,
    delivery_path: PathBuf,
    allow_outside: bool,
    worktree: Result<(), String>,
    now_ms: u64,
    ctx: &mut Ctx,
) {
    let path_str = delivery_path.display().to_string();
    match office::authorize(p, delivery_path, allow_outside) {
        Ok(()) => {
            match &worktree {
                Ok(()) => {
                    p.worktree_desks = true;
                    trace(p, now_ms, "desk", "worktree desks enabled — delivery is a git repo", ctx);
                }
                Err(reason) => {
                    p.worktree_desks = false;
                    trace(
                        p,
                        now_ms,
                        "desk",
                        format!("worktree desks unavailable ({}) — legacy desks", trace_preview(reason, 80)),
                        ctx,
                    );
                }
            }
            trace(p, now_ms, "authorize", format!("granted — {}", trace_preview(&path_str, 80)), ctx);
            ctx.dirty = true;
        }
        Err(e) => {
            // Report the ACTUAL phase — the old text hardcoded "stays in Ready"
            // even while Drafting, which sent the main agent chasing a phase that
            // did not exist (live-test 2026-07-15).
            let phase = match &p.phase {
                ProjectPhase::Drafting => "Drafting",
                ProjectPhase::Ready => "Ready",
                ProjectPhase::Running { .. } => "Running",
                ProjectPhase::Interrupted { .. } => "Interrupted",
                ProjectPhase::Halted { .. } => "Halted",
                ProjectPhase::Done { .. } => "Done",
            };
            let hint = if matches!(p.phase, ProjectPhase::Drafting) {
                " — the breakdown has not landed yet; wait for the board-is-ready notice before authorizing"
            } else {
                ""
            };
            let notice = format!("authorization refused: {:?}; project is in {}{}", e, phase, hint);
            trace(p, now_ms, "authorize", format!("refused ({e:?}) — project in {phase}"), ctx);
            queue_notice(p, now_ms, notice, ctx);
            ctx.dirty = true;
        }
    }
}

/// Apply an off-loop invoke result (5.1). Purpose-tagged so no persistent per-request
/// bookkeeping is needed — the kernel reacts to the result as an ordinary command.
fn invoke_result(p: &mut Project, purpose: InvokePurpose, outcome: Result<String, String>, now_ms: u64, ctx: &mut Ctx) {
    // Interrupt-from-drafting (feature): a persona/fold/breakdown/TRD/CRD/assume-check result that
    // was already in flight when the user interrupted must NOT advance the pipeline. The phase is
    // the guard — a stale result simply no-ops against Interrupted rather than needing a per-invoke
    // epoch. (In-flight WORKER results arrive as `HostEvent::Result`, not here, so a soft drain
    // still completes its running agents.)
    if matches!(p.phase, ProjectPhase::Interrupted) {
        trace(p, now_ms, "invoke", format!("ignored {:?} result — project interrupted", purpose), ctx);
        return;
    }
    match purpose {
        InvokePurpose::Triage => handle_triage_result(p, outcome, now_ms, ctx),
        InvokePurpose::Persona => {
            let reply = match outcome {
                Ok(t) => t,
                Err(_) => "The office did not answer in time; please try again.".to_string(),
            };
            p.office_transcript.push(ChatMsg {
                who: ChatAuthor::Office,
                text: reply.clone(),
            });
            // The trio is MAIN-CHAT-FIRST: every office drafting reply also goes out
            // through the chat.prompt outbox so the whole PRD conversation happens in
            // the koma chat (the panel transcript is the secondary surface). Long
            // replies are clipped for the notice — the full text is always in the
            // panel transcript.
            //
            // Drafting doc captures (design-speedup): a ```prd fence in a Drafting reply IS the PRD.
            // Land it, then start research IN PARALLEL (item 2, per `research_mode`) and run the PRD
            // safeguard gate at the SAME time; the TRD+CRD authoring join fires once both settle.
            // A chat-authored ```trd/```crd (a user pasting a revised doc) captures whichever is
            // present and runs the combined TRD+CRD gate. Without capture the persona chats forever
            // while the board stays empty (live-test 2026-07-15). A fenceless reply while
            // `pending_assumptions` is set re-runs the gate on the newest doc-set so the updated
            // transcript is re-judged.
            // Intake TRIAGE (feature: sdlc-triage): while the classifier is still in flight the track
            // — which decides WHICH fence/contract applies — is not yet known, so a Drafting reply
            // does NOT capture a doc into the pipeline (`!p.triage_pending`); it only flows to chat
            // below. Once the track is known, the capture path branches on it.
            if matches!(p.phase, ProjectPhase::Drafting) && !p.triage_pending {
                match p.track.as_str() {
                    // ENHANCEMENT: capture the ```change change-brief into the PRD slot (so the
                    // gate/research/JOIN machinery reuses unchanged), then run the SAME research + gate
                    // as a PRD; on clear the PostPrd join skips the TRD/CRD trio for a small breakdown.
                    "enhancement" => {
                        if let Some(cb) = office::extract_fenced(&reply, "change") {
                            p.prd_markdown = cb;
                            p.capture_nudge_count = 0;
                            p.gate_cleared = false;
                            trace(p, now_ms, "capture", format!("change-brief captured ({} bytes)", p.prd_markdown.len()), ctx);
                            queue_notice(
                                p,
                                now_ms,
                                format!(
                                    "office[{}]: change-brief drafted (full text in the Workflow panel) — checking assumptions before the small breakdown; do not authorize yet.",
                                    p.id.0
                                ),
                                ctx,
                            );
                            start_research_at_capture(p, now_ms, ctx);
                            gate_doc(p, Deferred::PostPrd, now_ms, ctx);
                            ctx.dirty = true;
                            return;
                        }
                        if !p.pending_assumptions.is_empty() {
                            recheck_pending_assumptions(p, now_ms, ctx);
                        }
                    }
                    // PATCH: the board is built programmatically at triage-resolve, so a Drafting reply
                    // never captures a doc — fall through to the chat notice.
                    "patch" => {}
                    // PROJECT (default): the unchanged full-ceremony capture path.
                    _ => {
                        if let Some(prd) = office::extract_prd(&reply) {
                            p.prd_markdown = prd;
                            p.capture_nudge_count = 0; // a successful capture resets the nudge cap
                            p.gate_cleared = false; // fresh doc-set: the PRD gate has not cleared yet
                            trace(p, now_ms, "capture", format!("PRD captured ({} bytes)", p.prd_markdown.len()), ctx);
                            queue_notice(
                                p,
                                now_ms,
                                format!(
                                    "office[{}]: PRD drafted (full text in the Workflow panel) — researching the stack and checking assumptions in parallel; do not authorize yet.",
                                    p.id.0
                                ),
                                ctx,
                            );
                            // Item 2/4: spawn research now (or defer/skip per research_mode), concurrently
                            // with the PRD gate below.
                            start_research_at_capture(p, now_ms, ctx);
                            gate_doc(p, Deferred::PostPrd, now_ms, ctx);
                            ctx.dirty = true;
                            return;
                        }
                        let (chat_trd, chat_crd) = office::extract_trd_crd(&reply);
                        if chat_trd.is_some() || chat_crd.is_some() {
                            reset_trdcrd_capture(p, now_ms, ctx);
                            p.capture_nudge_count = 0;
                            if let Some(t) = chat_trd {
                                p.trd_markdown = t;
                            }
                            if let Some(c) = chat_crd {
                                p.crd_markdown = c;
                            }
                            trace(
                                p,
                                now_ms,
                                "capture",
                                format!("TRD+CRD captured via chat (trd {}B, crd {}B)", p.trd_markdown.len(), p.crd_markdown.len()),
                                ctx,
                            );
                            queue_notice(p, now_ms, format!("office[{}]: TRD + clean-build requirements updated (panel).", p.id.0), ctx);
                            gate_doc(p, Deferred::Breakdown, now_ms, ctx);
                            ctx.dirty = true;
                            return;
                        }

                        // No fresh fence, but the pipeline is STOPPED on a prior gate's assumptions and the
                        // user just replied — their approval / answers / delegation now sit in the
                        // transcript. Re-run the gate on the newest captured doc so that UPDATED transcript
                        // is re-judged; a clean re-check clears `pending_assumptions` and resumes the
                        // deferred stage. Without this, a stopped gate never re-fires and Drafting wedges
                        // forever (live-test 2026-07-15: the persona answered in prose, no new fence, and
                        // the gate never re-ran). Exactly ONE re-check per persona exchange: the AssumeCheck
                        // result is not a persona result, so it cannot recurse. The persona reply itself
                        // still flows to chat below.
                        if !p.pending_assumptions.is_empty() {
                            recheck_pending_assumptions(p, now_ms, ctx);
                        }
                    }
                }
            }

            // A chat-authored ```trd in Ready is a RE-PLAN trigger, not a pipeline stage (6.2b):
            // capture it and point at workflow_breakdown. It does NOT run the gate or auto-run
            // CRD/breakdown — the deterministic pipeline is what drives those.
            if matches!(p.phase, ProjectPhase::Ready) {
                if let Some(trd) = office::extract_fenced(&reply, "trd") {
                    p.trd_markdown = trd;
                    trace(p, now_ms, "capture", format!("TRD updated in Ready ({} bytes)", p.trd_markdown.len()), ctx);
                    queue_notice(
                        p,
                        now_ms,
                        format!(
                            "office[{}]: TRD updated (panel). Run workflow_breakdown to re-plan the board from the revised TRD.",
                            p.id.0
                        ),
                        ctx,
                    );
                    ctx.dirty = true;
                    return;
                }
            }

            // Capture miss (feature: capture nudge): in Drafting, a long reply that landed no primary
            // doc fence while the doc slot is still empty is almost always a doc the office narrated
            // but forgot to fence (live-test 2026-07-15). Fire ONE deterministic re-invoke asking for
            // ONLY the fenced doc, capped at MAX_CAPTURE_NUDGES in a row. The doc is the PRD on the
            // project track and the change-brief (```change) on the enhancement track (feature:
            // sdlc-triage) — both live in the PRD slot. Never during triage (track unknown) and never
            // on the patch track (it drafts no doc); TRD/CRD are authored through dedicated invokes.
            if matches!(p.phase, ProjectPhase::Drafting)
                && !p.triage_pending
                && p.track != "patch"
                && p.prd_markdown.trim().is_empty()
                && reply.len() > PRD_NUDGE_MIN_REPLY_BYTES
                && p.capture_nudge_count < MAX_CAPTURE_NUDGES
            {
                p.capture_nudge_count += 1;
                let (doc_label, instruction) = if p.track == "enhancement" {
                    ("change-brief", CHANGE_NUDGE_INSTRUCTION)
                } else {
                    ("PRD", PRD_NUDGE_INSTRUCTION)
                };
                trace(p, now_ms, "nudge", format!("{doc_label} capture-miss nudge #{}", p.capture_nudge_count), ctx);
                let (mut system, prompt) = office::build_invoke(p, "");
                system.push_str(instruction);
                emit_draft_invoke(p, ctx, InvokePurpose::Persona, system, prompt);
                ctx.dirty = true;
                return;
            }

            let clipped = clip_notice(&reply);
            queue_notice(p, now_ms, format!("office[{}]: {}", p.id.0, clipped), ctx);
            ctx.dirty = true;
        }
        InvokePurpose::Fold => {
            if let Ok(summary) = outcome {
                office::apply_fold(p, summary);
                ctx.dirty = true;
            }
            // Fold done (or failed): re-issue the persona invoke. build_invoke hard-caps
            // the prompt, so proceeding un-folded on a fold failure is still safe.
            let (system, prompt) = office::build_invoke(p, "");
            emit_draft_invoke(p, ctx, InvokePurpose::Persona, system, prompt);
        }
        InvokePurpose::Breakdown => handle_breakdown_result(p, outcome, now_ms, ctx),
        InvokePurpose::BreakdownReask => handle_breakdown_reask_result(p, outcome, now_ms, ctx),
        InvokePurpose::BreakdownCompact => handle_breakdown_compact_result(p, outcome, now_ms, ctx),
        InvokePurpose::TrdCrd => handle_trdcrd_result(p, outcome, now_ms, ctx),
        InvokePurpose::AssumeCheckPrd => {
            handle_assume_check_result(p, Deferred::PostPrd, "PRD", PRD_TAGS, outcome, now_ms, ctx)
        }
        InvokePurpose::AssumeCheckTrdCrd => {
            handle_assume_check_result(p, Deferred::Breakdown, "TRD+CRD", TRDCRD_TAGS, outcome, now_ms, ctx)
        }
        InvokePurpose::AssumeResolve => handle_assume_resolve_result(p, outcome, now_ms, ctx),
        InvokePurpose::AssumeVerify => handle_assume_verify_result(p, outcome, now_ms, ctx),
    }
}

/// The FIRST breakdown attempt's result (6.3.2). `Ok` -> validate/land, or (parse failure)
/// re-ask once. `Err` whose text is a `models.invoke` timeout falls back to ONE compact
/// breakdown attempt instead of failing outright — by the time the kernel ever sees this
/// `Err`, the driver's own pool-level retry has ALREADY run and also timed out (`driver.rs`
/// `on_invoke_done` retries a timed-out invoke exactly once, reusing the same job/slot,
/// before ever routing the outcome here as `Command::InvokeResult`), so this is genuinely
/// the second consecutive timeout and a smaller ask is the deterministic next step. Any
/// other `Err` surfaces immediately, unchanged from before the compact ladder existed.
fn handle_breakdown_result(p: &mut Project, outcome: Result<String, String>, now_ms: u64, ctx: &mut Ctx) {
    let text = match outcome {
        Ok(t) => t,
        Err(e) => {
            if is_breakdown_timeout(&e) {
                let (system, prompt) = office::build_breakdown_prompt(p, None, true);
                emit_invoke(ctx, InvokePurpose::BreakdownCompact, &p.config.office_role, system, prompt);
            } else {
                queue_notice(p, now_ms, format!("office breakdown call failed: {e}"), ctx);
                ctx.dirty = true;
            }
            return;
        }
    };
    match office::parse_breakdown(&text) {
        Ok(breakdown) => apply_or_stash_breakdown(p, breakdown, text, now_ms, ctx),
        Err(e) => {
            let (system, prompt) = office::build_breakdown_prompt(p, Some(&format!("{e:?}")), false);
            emit_invoke(ctx, InvokePurpose::BreakdownReask, &p.config.office_role, system, prompt);
        }
    }
}

/// The single re-ask after a first-attempt parse failure (6.3.2), UNCHANGED by the compact
/// timeout ladder: `Ok` -> validate/land, or (second parse failure) surface a "rejected
/// twice" notice — the loop's hard stop. `Err` surfaces the same generic failure notice as
/// the first attempt; a re-ask never falls back to compact (only a FIRST-attempt timeout
/// does, in [`handle_breakdown_result`]).
fn handle_breakdown_reask_result(p: &mut Project, outcome: Result<String, String>, now_ms: u64, ctx: &mut Ctx) {
    let text = match outcome {
        Ok(t) => t,
        Err(e) => {
            queue_notice(p, now_ms, format!("office breakdown call failed: {e}"), ctx);
            ctx.dirty = true;
            return;
        }
    };
    match office::parse_breakdown(&text) {
        Ok(breakdown) => apply_or_stash_breakdown(p, breakdown, text, now_ms, ctx),
        Err(e) => {
            // Review finding (MINOR): "edit the board manually" is misleading in Drafting — the
            // board does not exist yet there (the breakdown is only STASHED early, applied at the
            // TRD+CRD JOIN; a permanently rejected breakdown just leaves it un-stashed, and nothing
            // auto-retries it — `authorize` only checks the phase transition, it does not re-run
            // the breakdown). Drafting gets a phase-aware message naming the ACTUAL next step
            // (`workflow_breakdown`); Ready+ (a manual `workflow_breakdown` re-plan, board already
            // exists) keeps the original wording.
            let msg = if matches!(p.phase, ProjectPhase::Drafting) {
                format!(
                    "office breakdown rejected twice ({e:?}); the board is not built yet — run workflow_breakdown to retry now"
                )
            } else {
                format!("office breakdown rejected twice ({e:?}); edit the board manually")
            };
            queue_notice(p, now_ms, msg, ctx);
            ctx.dirty = true;
        }
    }
}

/// The compact fallback's result (6.3.2 timeout ladder) — the one attempt
/// [`handle_breakdown_result`] issues after a first-attempt timeout. `Ok` -> validate/land
/// exactly like the normal path. Any failure here — timeout, other invoke error, or a parse
/// rejection — is terminal: there is no further kernel-level retry, so it surfaces the same
/// actionable notice either way, with a concrete next step for the user instead of silently
/// looping or dead-ending.
fn handle_breakdown_compact_result(p: &mut Project, outcome: Result<String, String>, now_ms: u64, ctx: &mut Ctx) {
    let text = match outcome {
        Ok(t) => t,
        Err(e) => {
            surface_compact_breakdown_failure(p, now_ms, format!("office breakdown call failed: {e}"), ctx);
            return;
        }
    };
    match office::parse_breakdown(&text) {
        Ok(breakdown) => apply_or_stash_breakdown(p, breakdown, text, now_ms, ctx),
        Err(e) => surface_compact_breakdown_failure(
            p,
            now_ms,
            format!("office breakdown (compact retry) rejected: {e:?}"),
            ctx,
        ),
    }
}

/// Route a validated breakdown by phase (design-speedup item 8). In Drafting the breakdown was
/// computed EARLY (parallel with the TRD+CRD gate verify): STASH its raw validated text and let the
/// JOIN (`maybe_apply_breakdown`) build the board once the gate clears. In Ready (a manual
/// `workflow_breakdown` re-plan) apply it immediately, replacing the board. `text` is the raw model
/// output that just validated, re-parsed at apply time.
fn apply_or_stash_breakdown(
    p: &mut Project,
    breakdown: office::Breakdown,
    text: String,
    now_ms: u64,
    ctx: &mut Ctx,
) {
    if matches!(p.phase, ProjectPhase::Drafting) {
        // SDLC escalation (feature: sdlc-triage): an ENHANCEMENT whose breakdown returns more than 3
        // tasks is wider than a change — escalate to the full project ceremony (author TRD+CRD)
        // instead of stashing this plan. Pre-authorize only (we are in Drafting).
        if p.track == "enhancement" && breakdown.task_count() > ENHANCEMENT_BREAKDOWN_TASK_MAX {
            escalate_enhancement_to_project_via_breakdown(p, breakdown.task_count(), now_ms, ctx);
            return;
        }
        p.pending_breakdown = Some(text);
        trace(p, now_ms, "breakdown", "breakdown stashed (early)", ctx);
        maybe_apply_breakdown(p, now_ms, ctx);
        ctx.dirty = true;
    } else {
        land_breakdown(p, breakdown, now_ms, ctx);
    }
}

/// Escalate an ENHANCEMENT to the full project track because its breakdown was wider than a change
/// (feature: sdlc-triage). Flip the track, discard the oversized plan and the placeholder hygiene
/// CRD, and author the real TRD+CRD from the change-brief (which stays in the PRD slot as context).
/// The change-brief/PRD gate already cleared, so authoring can fire immediately.
fn escalate_enhancement_to_project_via_breakdown(p: &mut Project, task_count: usize, now_ms: u64, ctx: &mut Ctx) {
    trace(p, now_ms, "sdlc", "escalating enhancement → project", ctx);
    queue_notice(
        p,
        now_ms,
        format!(
            "office[{}]: the change needs {} tasks — wider than an enhancement; reclassified as a full project, drafting TRD + CRD.",
            p.id.0, task_count
        ),
        ctx,
    );
    p.track = "project".to_string();
    p.pending_breakdown = None;
    // The placeholder hygiene CRD (and any partial TRD) is dropped — the project ceremony authors the
    // real pair. `gate_cleared` stays true (the change-brief/PRD gate passed), so authoring fires now.
    p.crd_markdown.clear();
    p.trd_markdown.clear();
    start_trdcrd_invoke(p, now_ms, ctx);
    ctx.dirty = true;
}

/// Land a validated breakdown on the board and announce it — shared by the first attempt,
/// the re-ask, and the compact fallback, since every successful path lands identically. THE
/// authorize invitation lives HERE, after tasks really exist — never at PRD capture
/// (live-test 2026-07-15: an early nudge sent the main agent into authorize/WrongPhase retry
/// loops while the breakdown was still generating).
fn land_breakdown(p: &mut Project, breakdown: office::Breakdown, now_ms: u64, ctx: &mut Ctx) {
    office::apply_breakdown(p, breakdown);
    let epics = p.epics.len();
    let tasks = p.tasks.len();
    trace(p, now_ms, "breakdown", format!("accepted — {tasks} task(s), {epics} epic(s)"), ctx);
    queue_notice(
        p,
        now_ms,
        format!(
            "office[{}]: board is ready — {} task{} across {} epic{}. Authorize with a delivery path (workflow_authorize) to start the production line.",
            p.id.0,
            tasks,
            if tasks == 1 { "" } else { "s" },
            epics,
            if epics == 1 { "" } else { "s" }
        ),
        ctx,
    );
    ctx.dirty = true;
}

/// Surface the compact fallback's terminal failure with a concrete next step (6.3.2): unlike
/// the first attempt's one-shot fallback to compact, nothing after the compact attempt
/// retries automatically — the user must act.
fn surface_compact_breakdown_failure(p: &mut Project, now_ms: u64, base: String, ctx: &mut Ctx) {
    queue_notice(
        p,
        now_ms,
        format!(
            "{base}; try workflow_breakdown to retry, or bind a faster model to the office role in the koma sidebar"
        ),
        ctx,
    );
    ctx.dirty = true;
}

/// Whether a breakdown invoke error string is the host's model-call timeout (broker inner 330s
/// / wire 360s, wire.rs `EXT_MODELS_CALL_TIMEOUT`) — the one class
/// [`handle_breakdown_result`] falls back to a compact attempt for. Mirrors the driver's own
/// `is_invoke_timeout` (`office-daemon/driver.rs`), duplicated here since the kernel crate
/// has no dependency on the daemon crate.
fn is_breakdown_timeout(err: &str) -> bool {
    err.contains("timed out") || err.contains("timeout")
}

/// Emit an `InvokeModel` effect (no model override — resolve the role's model). `req_id` is a
/// placeholder (0) — the driver mints the real id when it hands the job to the off-loop invoke pool
/// (5.1); the kernel matches results by `purpose`, not id.
fn emit_invoke(ctx: &mut Ctx, purpose: InvokePurpose, role: &str, system: String, prompt: String) {
    ctx.fx.push(Effect::InvokeModel {
        req_id: 0,
        purpose,
        role: role.to_string(),
        model: None,
        system,
        prompt,
        format: invoke_format(purpose),
    });
}

/// Emit a DOC-DRAFTING invoke (design-speedup item 4): the persona reply, the TRD+CRD authoring,
/// and the ask-mode auto-resolve rewrite. Runs on the `office_role`, but carries `drafter_model` as
/// the model override when the project set one (mirroring `worker_model`/`reviewer_model` on spawns).
/// The gate/safeguard checks keep using their roles with no override, so they stay on the fast model.
fn emit_draft_invoke(p: &Project, ctx: &mut Ctx, purpose: InvokePurpose, system: String, prompt: String) {
    ctx.fx.push(Effect::InvokeModel {
        req_id: 0,
        purpose,
        role: p.config.office_role.clone(),
        model: p.config.drafter_model.clone(),
        system,
        prompt,
        format: invoke_format(purpose),
    });
}

/// The `models.invoke` output format for a purpose (feature 5). `Some("json")` ONLY for the
/// breakdown family, whose prompts genuinely demand a JSON plan — the host maps it to a
/// chat-completions `response_format: json_object` (other dialects ignore it).
///
/// The assume-check gate is deliberately NOT in the json set: its prompt asks for the
/// `ASSUME-CHECK` TEXT block, and forcing json mode there makes chat-completions dialects
/// either 400 (OpenAI requires the word "json" in the prompt) or emit a JSON object the
/// tolerant text parser rejects — and since the safeguard fails OPEN on an unparseable
/// result, json mode would silently disable the safeguard on the most common dialects.
fn invoke_format(purpose: InvokePurpose) -> Option<&'static str> {
    match purpose {
        InvokePurpose::Breakdown | InvokePurpose::BreakdownReask | InvokePurpose::BreakdownCompact => {
            Some("json")
        }
        // Triage asks for the `SDLC-TRIAGE` TEXT block, so it stays OUT of json mode for the same
        // reason as the assume-check gate (json mode 400s / breaks the tolerant text parser on the
        // common chat-completions dialects) — and it fails OPEN to `project` on an unparseable result.
        InvokePurpose::Triage
        | InvokePurpose::Persona
        | InvokePurpose::TrdCrd
        | InvokePurpose::Fold
        | InvokePurpose::AssumeCheckPrd
        | InvokePurpose::AssumeCheckTrdCrd
        // AssumeResolve / AssumeVerify re-emit / report prose text blocks, never JSON.
        | InvokePurpose::AssumeResolve
        | InvokePurpose::AssumeVerify => None,
    }
}

/// Hard interrupt (default): stop dispatch, kill every tracked binding, normalize
/// in-flight tasks. Workers -> Todo (attempt preserved, not a bounce); reviewers ->
/// Review{None} (reviewer respawns on resume). Desks are retained (5.5).
fn hard_interrupt(p: &mut Project, now_ms: u64, ctx: &mut Ctx) {
    let from = p.phase.clone();
    if let Ok(ph) = step_project(&p.phase, ProjectTransition::Interrupt) {
        // Remember where we came from so resume returns to the right phase (a Drafting-interrupt
        // resumes back to Drafting, not forward to Running).
        p.interrupted_from = Some(from.clone());
        p.phase = ph;
        trace(p, now_ms, "phase", format!("hard interrupt from {}", phase_label(&from)), ctx);
    }
    // A gate invoke in flight at interrupt time never gets to clear `gate_invoke_live_hint`: its
    // eventual `InvokeResult` is unconditionally dropped by the `Interrupted` guard in
    // `invoke_result` (never reaches `run_gate_cleared`/`freeze_critical`). Left `true`, it would
    // wrongly block `self_heal_stale_prd_gate` after resume respawns a FRESH (young) researcher —
    // exactly the wedge this hint exists to prevent. Clear it here: nothing can still be in flight
    // for THIS process once Interrupted, by the same "process boundary" reasoning as the on-load
    // default.
    p.gate_invoke_live_hint = false;
    // Intake triage (feature: sdlc-triage): same "process boundary" reasoning — a triage invoke in
    // flight at interrupt has its result dropped by the `Interrupted` guard in `invoke_result`, so a
    // `triage_pending` left `true` would suppress the persona doc-capture forever after resume. Clear
    // it; the track stays whatever it was (default "project"), which is the safe fallback.
    p.triage_pending = false;
    // Cut off the project-level drafting/completion analysts (research 6.2b, audit 6.2c). They
    // are NOT task bindings, so the normalization loop below never touches them; a dangling
    // researcher/auditor would keep burning tokens against an interrupted project (feature:
    // interrupt-from-drafting).
    kill_project_bindings(p, now_ms, ctx);
    for t in p.tasks.iter_mut() {
        match &t.state {
            TaskState::OnProgress { binding, attempt } => {
                let attempt = *attempt;
                if binding.ext_agent_id != PROVISIONAL {
                    ctx.fx.push(Effect::Kill {
                        ext_agent_id: binding.ext_agent_id,
                    });
                }
                set_next_attempt(t, now_ms, attempt);
                record(t, now_ms, "interrupt-hard");
                t.state = TaskState::Todo;
            }
            TaskState::Review {
                binding: Some(b),
                attempt,
            } => {
                let attempt = *attempt;
                if b.ext_agent_id != PROVISIONAL {
                    ctx.fx.push(Effect::Kill {
                        ext_agent_id: b.ext_agent_id,
                    });
                }
                record(t, now_ms, "interrupt-hard");
                t.state = TaskState::Review {
                    binding: None,
                    attempt,
                };
            }
            _ => {}
        }
    }
    ctx.dirty = true;
}

/// Soft drain: stop dispatching new work; leave in-flight agents alone. Phase moves
/// to Interrupted immediately, which halts the dispatch scan, but completion events
/// keep flowing so running agents finish and their results are processed (5.5). Unlike
/// [`hard_interrupt`] it does NOT kill the analyst bindings — a drain lets in-flight work
/// finish — but it still records `interrupted_from` so resume returns to the right phase.
fn soft_interrupt(p: &mut Project, now_ms: u64, ctx: &mut Ctx) {
    let from = p.phase.clone();
    if let Ok(ph) = step_project(&p.phase, ProjectTransition::Interrupt) {
        p.interrupted_from = Some(from.clone());
        p.phase = ph;
        trace(p, now_ms, "phase", format!("soft drain from {}", phase_label(&from)), ctx);
    }
    // Same reasoning as `hard_interrupt`: the `Interrupted` guard in `invoke_result` drops a gate
    // invoke's result unconditionally (soft drain does not exempt kernel-level invokes, only
    // sub-agent `HostEvent::Result`s), so a hint left `true` would never clear on its own.
    p.gate_invoke_live_hint = false;
    // Intake triage (feature: sdlc-triage): clear the pending flag for the same reason.
    p.triage_pending = false;
}

/// Kill the project-level analyst bindings (research 6.2b / audit 6.2c) on a hard interrupt
/// (feature: interrupt-from-drafting). Project-level, not task bindings, so the task-normalization
/// loop never touches them. A real (non-provisional) id gets a `Kill` effect; the binding is
/// cleared either way so a late `agents.done`/result no-ops (`research_bound_to`/`audit_bound_to`
/// stop matching once the binding is gone).
fn kill_project_bindings(p: &mut Project, now_ms: u64, ctx: &mut Ctx) {
    if let Some(b) = p.research.take() {
        if b.ext_agent_id != PROVISIONAL {
            ctx.fx.push(Effect::Kill { ext_agent_id: b.ext_agent_id });
        }
        trace(p, now_ms, "research", "killed on interrupt", ctx);
    }
    if let Some(b) = p.audit.take() {
        if b.ext_agent_id != PROVISIONAL {
            ctx.fx.push(Effect::Kill { ext_agent_id: b.ext_agent_id });
        }
        trace(p, now_ms, "audit", "killed on interrupt", ctx);
    }
}

// ---------------------------------------------------------------------------
// Host event handling
// ---------------------------------------------------------------------------

fn handle_event(p: &mut Project, e: HostEvent, now_ms: u64, ctx: &mut Ctx) {
    match e {
        HostEvent::Tick => {} // dispatch scan runs after every input
        HostEvent::Reconcile => runtime_ceiling(p, now_ms, ctx),
        HostEvent::Spawned {
            task,
            agent_id,
            spawned_at_ms,
        } => on_spawned(p, &task, agent_id, spawned_at_ms, now_ms, ctx),
        HostEvent::AgentsDone { agent_id, status, error } => {
            on_agents_done(p, agent_id, &status, error.as_deref(), now_ms, ctx)
        }
        HostEvent::Result { agent_id, text } => on_result(p, agent_id, text, now_ms, ctx),
        HostEvent::SpawnFailed { task, reason } => on_spawn_failed(p, &task, reason, now_ms, ctx),
        HostEvent::ResearchSpawned { agent_id, spawned_at_ms } => {
            on_research_spawned(p, agent_id, spawned_at_ms, ctx)
        }
        HostEvent::ResearchFailed { reason } => research_degrade(p, reason, now_ms, ctx),
        HostEvent::AuditSpawned { agent_id, spawned_at_ms } => {
            on_audit_spawned(p, agent_id, spawned_at_ms, ctx)
        }
        HostEvent::AuditFailed { reason } => audit_degrade(p, reason, now_ms, ctx),
        HostEvent::CommentDelivered { task, comment_id } => {
            on_comment_delivered(p, &task, comment_id, now_ms, ctx)
        }
        HostEvent::DeskMerged { task } => on_desk_merged(p, &task, now_ms, ctx),
        HostEvent::DeskMergeConflict { task, summary, is_conflict } => {
            on_desk_merge_conflict(p, &task, summary, is_conflict, now_ms, ctx)
        }
    }
}

/// Record the real agent id onto a provisional binding.
fn on_spawned(p: &mut Project, task: &TaskId, agent_id: u64, spawned_at_ms: u64, now_ms: u64, ctx: &mut Ctx) {
    if let Some(idx) = find_task(p, task) {
        let bound = match &mut p.tasks[idx].state {
            TaskState::OnProgress { binding, .. } => Some(binding),
            TaskState::Review {
                binding: Some(b), ..
            } => Some(b),
            _ => None,
        };
        if let Some(b) = bound {
            b.ext_agent_id = agent_id;
            b.spawned_at_ms = spawned_at_ms;
            record(&mut p.tasks[idx], now_ms, format!("spawned:{}", agent_id));
            ctx.dirty = true;
        }
    }
}

/// Build a binding-failure reason from a terminal `status` plus koma's optional additive
/// `error` text (feature C): `"<who> <status>: <error>"` when error text is present, else
/// `"<who> <status>"` (old komas, and the driver's own `agents.status`-poll path, send none).
fn degrade_reason(who: &str, status: &str, error: Option<&str>) -> String {
    match error {
        Some(e) if !e.is_empty() => format!("{who} {status}: {e}"),
        _ => format!("{who} {status}"),
    }
}

/// Terminal host status. `done` -> fetch the report (no state change yet);
/// `error`/`killed`/anything else -> re-queue the task (worker -> Todo attempt++,
/// reviewer -> Review{None}). `error` is koma's optional failure text, folded into the
/// project-level research/audit degrade reason when present.
fn on_agents_done(p: &mut Project, agent_id: u64, status: &str, error: Option<&str>, now_ms: u64, ctx: &mut Ctx) {
    // Research binding (6.2b) is project-level, checked before the task bindings. `done` ->
    // fetch the findings (existing FetchResult path); anything else is a dead researcher and
    // degrades exactly like a spawn failure (never wedges Drafting).
    if research_bound_to(p, agent_id) {
        if status.eq_ignore_ascii_case("done") {
            ctx.fx.push(Effect::FetchResult { ext_agent_id: agent_id });
        } else {
            research_degrade(p, degrade_reason("researcher", status, error), now_ms, ctx);
        }
        return;
    }
    // The clean-build auditor binding (6.2c) is project-level like research: `done` fetches the
    // OFFICE-AUDIT verdict; anything else is a dead auditor and degrades to Done (never wedges).
    if audit_bound_to(p, agent_id) {
        if status.eq_ignore_ascii_case("done") {
            ctx.fx.push(Effect::FetchResult { ext_agent_id: agent_id });
        } else {
            audit_degrade(p, degrade_reason("auditor", status, error), now_ms, ctx);
        }
        return;
    }
    let idx = match find_by_agent(p, agent_id) {
        Some(i) => i,
        None => return,
    };
    if status.eq_ignore_ascii_case("done") {
        ctx.fx.push(Effect::FetchResult {
            ext_agent_id: agent_id,
        });
    } else {
        diagnose_and_requeue(p, idx, status, error, now_ms, ctx);
    }
}

/// Milliseconds below which a dead agent counts as an INSTANT death (item 4): it fell over almost
/// immediately after spawn, so a blind same-millisecond re-dispatch would just replay the death.
const INSTANT_DEATH_MS: u64 = 5_000;

/// Diagnose a dead worker/reviewer (an `agents.done` with a non-`done` status, or a killed poll)
/// and re-queue it (item 4). Three cases:
///  - the death reason names a "daemon restart" -> PAUSE without burning an attempt or backoff; the
///    existing reconcile/resume flow re-dispatches it (a restart is transient, not the task's fault).
///  - it died < `INSTANT_DEATH_MS` after spawn -> INSTANT death: record the reason + trace
///    "attempt N died at step 0", then set a dispatch backoff (10s then 60s) before the retry.
///  - otherwise (it ran a while, then died) -> re-queue immediately as before, clearing any backoff.
fn diagnose_and_requeue(p: &mut Project, idx: usize, status: &str, error: Option<&str>, now_ms: u64, ctx: &mut Ctx) {
    let who = match binding_kind(&p.tasks[idx].state) {
        Some(AgentKind::Reviewer) => "reviewer",
        _ => "worker",
    };
    let reason = degrade_reason(who, status, error);

    // Daemon-restart deaths are host lifecycle noise, not task failures: don't burn an attempt.
    if is_daemon_restart(&reason) {
        pause_for_daemon_restart(p, idx, now_ms, ctx);
        return;
    }

    let alive = spawned_ago(&p.tasks[idx].state, now_ms);
    let instant = matches!(alive, Some(ms) if ms < INSTANT_DEATH_MS);
    let attempt = current_attempt(&p.tasks[idx].state);

    if instant {
        // `prior` counts instant deaths already recorded, so the FIRST instant death backs off 10s
        // (going into the 2nd attempt) and the second 60s (going into the 3rd), matching the spec.
        let prior = instant_death_count(&p.tasks[idx]);
        record(&mut p.tasks[idx], now_ms, format!("died-at-step-0:{reason}"));
        trace(
            p,
            now_ms,
            "death",
            format!("attempt {attempt} died at step 0: {}", trace_preview(&reason, 80)),
            ctx,
        );
        ctx.dirty = true;

        // item 3: chronic instant death — 3 in a row (this one included) parks the task instead of
        // retrying forever, mirroring the SpawnFailed pattern (kernel.rs `on_spawn_failed`).
        // `instant_death_streak` counts CONSECUTIVE instant deaths and resets on a non-instant run,
        // unlike `prior`/`instant_death_count` above, which is a lifetime tally that only escalates
        // the backoff and never resets.
        if instant_death_streak(&p.tasks[idx]) >= 3 {
            // Same reasoning as the diff_stat gate below: a reviewer death never touched the
            // worktree, so only reclaim the desk for a chronically-dying WORKER.
            if who == "worker" {
                maybe_remove_worktree(p, idx, ctx);
            }
            p.tasks[idx].state = TaskState::Parked {
                reason: ParkReason::InstantDeath(reason.clone()),
                attempt,
            };
            trace(
                p,
                now_ms,
                "death",
                format!(
                    "{} parked (chronic instant death): {}",
                    short_task(&p.tasks[idx].id),
                    trace_preview(&reason, 80)
                ),
                ctx,
            );
            check_halt(p, now_ms, ctx);
            return;
        }

        let delay = instant_death_backoff_ms(prior);
        // Re-queue FIRST (moves the task to Todo / Review{None}), then stamp the cooldown on it so
        // the next dispatch scan defers the retry.
        requeue_failed(p, idx, now_ms, "worker-error", ctx);
        p.tasks[idx].dispatch_after_ms = now_ms.saturating_add(delay);
        // item 1: only a WORKER death touches the worktree — a reviewer never writes to it. Clearing
        // diff_stat on a reviewer death would permanently wedge the task: worktree-mode review
        // dispatch (`pending_reviews_sorted`) gates on `diff_stat.is_some()`, and the only writer is
        // the worker's commit step (driver.rs `maybe_commit_worktree`), which a reviewer never runs.
        if who == "worker" {
            p.tasks[idx].diff_stat = None; // a fresh worktree recomputes it on the next commit
        }
        trace(
            p,
            now_ms,
            "death",
            format!("retry deferred {}s (instant-death backoff)", delay / 1000),
            ctx,
        );
    } else {
        // item 3: an explicit "ran a while" marker so `instant_death_streak` can find the reset
        // boundary — distinct from the requeue's own "worker-error" tag below, which both the
        // instant and non-instant paths share and so can't be used to tell them apart.
        record(&mut p.tasks[idx], now_ms, format!("died-after-run:{reason}"));
        requeue_failed(p, idx, now_ms, "worker-error", ctx);
        p.tasks[idx].dispatch_after_ms = 0; // ran a while; retry immediately as before
        // item 1: see the instant-death branch above — only a worker death clears diff_stat.
        if who == "worker" {
            p.tasks[idx].diff_stat = None;
        }
    }
}

/// The instant-death retry backoff for the NEXT attempt, given how many instant deaths already
/// happened (item 4): first death -> 10s (attempt 2), second onward -> 60s (attempt 3+ cap). No
/// backoff would be `0`.
fn instant_death_backoff_ms(prior_instant_deaths: u32) -> u64 {
    match prior_instant_deaths {
        0 => 10_000,
        _ => 60_000,
    }
}

/// Count of `died-at-step-0` markers already in a task's history (item 4) — the instant-death
/// tally that escalates the backoff. Distinct from `spawn_failure_streak`: a spawn succeeds each
/// retry (recording `spawned:`), so this counts the deaths themselves, not a since-last-spawn run.
/// A LIFETIME tally that never resets — for the CONSECUTIVE streak used to park chronic instant
/// deaths (item 3), see [`instant_death_streak`] instead.
fn instant_death_count(t: &Task) -> u32 {
    t.history.iter().filter(|e| e.event.starts_with("died-at-step-0")).count() as u32
}

/// Consecutive instant-death streak, most-recent-first (item 3, chronic-death parking): the number
/// of instant deaths in a row since the task last proved it isn't chronically instant-dying — a
/// non-instant death (`died-after-run`), a spawn failure, a runtime-ceiling kill, a daemon-restart
/// pause, or a normal report/review outcome all reset it. Bookkeeping markers that decorate every
/// death alike (`next-attempt:`, `spawned:`, the requeue's own `worker-error` tag) are transparent:
/// they neither count nor break the streak.
fn instant_death_streak(t: &Task) -> u32 {
    let mut count = 0;
    for e in t.history.iter().rev() {
        if e.event.starts_with("died-at-step-0") {
            count += 1;
        } else if e.event.starts_with("next-attempt:") || e.event.starts_with("spawned:") || e.event.starts_with("worker-error") {
            continue;
        } else {
            break;
        }
    }
    count
}

/// Whether a death reason names a daemon restart (item 4): the host tore the agent down because the
/// daemon itself cycled, not because the task misbehaved — so we should not spend an attempt on it.
fn is_daemon_restart(reason: &str) -> bool {
    reason.to_ascii_lowercase().contains("daemon restart")
}

/// Milliseconds a task's agent has been alive, from its binding's spawn time (item 4). `None` when
/// the task carries no live binding (nothing to measure).
fn spawned_ago(state: &TaskState, now_ms: u64) -> Option<u64> {
    let at = match state {
        TaskState::OnProgress { binding, .. } => binding.spawned_at_ms,
        TaskState::Review { binding: Some(b), .. } => b.spawned_at_ms,
        _ => return None,
    };
    Some(now_ms.saturating_sub(at))
}

/// The attempt number a task is currently running (item 4), for the death trace.
fn current_attempt(state: &TaskState) -> u32 {
    match state {
        TaskState::OnProgress { attempt, .. } | TaskState::Review { attempt, .. } => *attempt,
        _ => 0,
    }
}

/// Pause a task after a daemon-restart death (item 4): re-queue WITHOUT incrementing the attempt or
/// applying backoff, so the existing reconcile/resume flow re-dispatches it cleanly. A worker returns
/// to Todo at the SAME attempt; a reviewer to Review{None}.
fn pause_for_daemon_restart(p: &mut Project, idx: usize, now_ms: u64, ctx: &mut Ctx) {
    record(&mut p.tasks[idx], now_ms, "paused:daemon-restart");
    trace(
        p,
        now_ms,
        "death",
        format!("{} paused (daemon restart) — awaiting redispatch", short_task(&p.tasks[idx].id)),
        ctx,
    );
    match &p.tasks[idx].state {
        TaskState::OnProgress { attempt, .. } => {
            let attempt = *attempt;
            // Preserve the attempt (re-stamp the SAME number, never +1).
            set_next_attempt(&mut p.tasks[idx], now_ms, attempt);
            p.tasks[idx].state = TaskState::Todo;
            // item 1: only a WORKER binding ever touches the worktree, so only a worker's restart
            // clears diff_stat. A reviewer restarting into Review{None} keeps its diff_stat — clearing
            // it here would permanently wedge the task, since `pending_reviews_sorted` gates
            // worktree-mode review dispatch on `diff_stat.is_some()` and nothing else ever sets it.
            p.tasks[idx].diff_stat = None; // a fresh worktree recomputes it on the next commit
        }
        TaskState::Review { attempt, .. } => {
            let attempt = *attempt;
            p.tasks[idx].state = TaskState::Review { binding: None, attempt };
        }
        _ => {}
    }
    p.tasks[idx].dispatch_after_ms = 0;
    ctx.dirty = true;
}

/// A fetched terminal report. Dispatch to the worker or reviewer path by binding kind.
fn on_result(p: &mut Project, agent_id: u64, text: String, now_ms: u64, ctx: &mut Ctx) {
    // Research findings (6.2b) + audit verdict (6.2c) route to their project-level handlers,
    // before the task lookup.
    if research_bound_to(p, agent_id) {
        on_research_result(p, text, now_ms, ctx);
        return;
    }
    if audit_bound_to(p, agent_id) {
        on_audit_result(p, text, now_ms, ctx);
        return;
    }
    let idx = match find_by_agent(p, agent_id) {
        Some(i) => i,
        None => return,
    };
    match binding_kind(&p.tasks[idx].state) {
        Some(AgentKind::Worker) => on_worker_result(p, idx, text, now_ms, ctx),
        Some(AgentKind::Reviewer) => on_reviewer_result(p, idx, text, now_ms, ctx),
        // A task binding is only ever Worker/Reviewer; Researcher/Auditor are project-level and
        // never reach this task path (they route above, in `on_result`).
        Some(AgentKind::Researcher) | Some(AgentKind::Auditor) | None => {}
    }
}

/// Worker report: `complete`/unparseable -> Review (reviewer spawns on the next
/// dispatch scan); `blocked` -> Parked(WorkerBlocked) + halt check. Comment ACKs in
/// the trailer flip receipts to Read (only from a prior Delivered).
fn on_worker_result(p: &mut Project, idx: usize, text: String, now_ms: u64, ctx: &mut Ctx) {
    let attempt = match &p.tasks[idx].state {
        TaskState::OnProgress { attempt, .. } => *attempt,
        _ => return,
    };
    let rep = report::parse_report(&text);
    apply_acks(&mut p.tasks[idx], &rep.ack_comments, now_ms);
    p.tasks[idx].last_report = Some(text);

    match rep.status {
        ReportStatus::Complete | ReportStatus::Unparseable => {
            let tag = if rep.status == ReportStatus::Complete {
                "report:complete"
            } else {
                "report:unparseable"
            };
            record(&mut p.tasks[idx], now_ms, tag);
            p.tasks[idx].state = TaskState::Review {
                binding: None,
                attempt,
            };
            let word = if rep.status == ReportStatus::Complete { "complete" } else { "unparseable" };
            let label = format!("{} → review ({word})", short_task(&p.tasks[idx].id));
            trace(p, now_ms, "task", label, ctx);
            ctx.dirty = true;
        }
        ReportStatus::Blocked => {
            let reason = rep.blocked_reason.unwrap_or_default();
            record(&mut p.tasks[idx], now_ms, "report:blocked");
            p.tasks[idx].state = TaskState::Parked {
                reason: ParkReason::WorkerBlocked(reason),
                attempt,
            };
            let label = format!("{} → parked (worker blocked)", short_task(&p.tasks[idx].id));
            trace(p, now_ms, "task", label, ctx);
            ctx.dirty = true;
            check_halt(p, now_ms, ctx);
        }
    }
}

/// Reviewer verdict: `pass` -> Done (+ project-complete check); `fail`/unparseable ->
/// bounces++, notes stored for the next worker prompt, re-queue or (over budget)
/// escalate with a chat.prompt nudge + Parked(ReviewBounceBudget) + halt check.
fn on_reviewer_result(p: &mut Project, idx: usize, text: String, now_ms: u64, ctx: &mut Ctx) {
    let attempt = match &p.tasks[idx].state {
        TaskState::Review { attempt, .. } => *attempt,
        _ => return,
    };
    let rev = report::parse_review(&text);

    match rev.verdict {
        Verdict::Pass => {
            p.tasks[idx].last_review = rev.reasons.clone().or_else(|| Some(text.clone()));
            record(&mut p.tasks[idx], now_ms, "review:pass");
            ctx.dirty = true;
            // Rolling score (item 3): fold this pass's hygiene grade into the running average; an
            // absent `hygiene:` line counts as 100 (compat). Done BEFORE the merge so the sag trace
            // lands with the pass.
            accumulate_hygiene(p, rev.hygiene.unwrap_or(100), now_ms, ctx);

            if p.worktree_desks {
                // Worktree desks (item 1): don't complete yet — merge the task branch into main.
                // A clean merge -> Done (via `on_desk_merged`); a conflict -> bounce.
                match desk_git_paths(p, idx) {
                    Some((repo, desk, branch)) => {
                        // The reviewer is finished; drop its binding and park in the merge wait so
                        // the slot frees and no reviewer re-dispatches (gated by `awaiting_merge`).
                        p.tasks[idx].state = TaskState::Review { binding: None, attempt };
                        p.tasks[idx].awaiting_merge = true;
                        ctx.fx.push(Effect::MergeDesk {
                            task: p.tasks[idx].id.clone(),
                            repo,
                            desk,
                            branch,
                        });
                        let label = format!("{} passed — merging task branch", short_task(&p.tasks[idx].id));
                        trace(p, now_ms, "desk", label, ctx);
                    }
                    // Worktree mode but no desk path recorded (shouldn't happen): degrade to a plain
                    // completion so the line never wedges.
                    None => complete_passed_task(p, idx, now_ms, ctx),
                }
            } else {
                complete_passed_task(p, idx, now_ms, ctx);
            }
        }
        Verdict::Fail | Verdict::Unparseable => {
            let note = rev.reasons.unwrap_or(text);
            bounce_task(p, idx, attempt, note, now_ms, ctx);
        }
    }
}

/// Complete a task whose work is on the main branch (a legacy review pass, or a clean worktree
/// merge): move it Done, reclaim its worktree (worktree mode, unless `keep_desks`), and run the
/// project-completion + halt checks. Shared so both paths behave identically.
fn complete_passed_task(p: &mut Project, idx: usize, now_ms: u64, ctx: &mut Ctx) {
    p.tasks[idx].state = TaskState::Done { at_ms: now_ms };
    let label = format!("{} → done (review pass)", short_task(&p.tasks[idx].id));
    trace(p, now_ms, "task", label, ctx);
    maybe_remove_worktree(p, idx, ctx);
    ctx.dirty = true;
    maybe_complete_project(p, now_ms, ctx);
    check_halt(p, now_ms, ctx);
}

/// Bounce a task back for another attempt — shared by a review FAIL/unparseable and a worktree
/// MERGE CONFLICT (item 1). `note` becomes the review note the next worker prompt carries. Within
/// budget -> Todo (attempt++); over budget -> Parked(ReviewBounceBudget) + reclaim the worktree
/// (unless `keep_desks`) + halt check. `diff_stat` is cleared so a retry recomputes it fresh.
fn bounce_task(p: &mut Project, idx: usize, attempt: u32, note: String, now_ms: u64, ctx: &mut Ctx) {
    p.tasks[idx].bounces += 1;
    p.tasks[idx].last_review = Some(note);
    p.tasks[idx].diff_stat = None;
    record(&mut p.tasks[idx], now_ms, "review:fail");
    ctx.dirty = true;

    // SDLC escalation (feature: sdlc-triage): a PATCH whose single task bounces twice is wider than a
    // patch — convert the track to enhancement and re-dispatch (do NOT park), giving the task another
    // life under the richer framing before its next attempt. Running-time relabel, no re-drafting.
    // Fires once: the track is no longer "patch" afterward.
    if p.track == "patch" && p.tasks[idx].bounces >= PATCH_BOUNCE_ESCALATION {
        p.track = "enhancement".to_string();
        trace(p, now_ms, "sdlc", "escalating patch → enhancement", ctx);
        queue_notice(
            p,
            now_ms,
            format!(
                "office[{}]: patch bounced {} times — escalating to an enhancement; re-dispatching.",
                p.id.0, p.tasks[idx].bounces
            ),
            ctx,
        );
        set_next_attempt(&mut p.tasks[idx], now_ms, attempt + 1);
        p.tasks[idx].state = TaskState::Todo;
        let label = format!("{} → todo (patch escalation)", short_task(&p.tasks[idx].id));
        trace(p, now_ms, "task", label, ctx);
        return;
    }

    if p.tasks[idx].bounces > p.config.bounce_budget {
        let notice = format!(
            "production line: task {} '{}' exceeded the review bounce budget; the office parked it. Advise or edit the board.",
            p.tasks[idx].id.0, p.tasks[idx].title
        );
        queue_notice(p, now_ms, notice, ctx);
        maybe_remove_worktree(p, idx, ctx);
        p.tasks[idx].state = TaskState::Parked {
            reason: ParkReason::ReviewBounceBudget,
            attempt,
        };
        let label = format!("{} → parked (bounce budget)", short_task(&p.tasks[idx].id));
        trace(p, now_ms, "task", label, ctx);
        check_halt(p, now_ms, ctx);
    } else {
        set_next_attempt(&mut p.tasks[idx], now_ms, attempt + 1);
        p.tasks[idx].state = TaskState::Todo;
        let label = format!("{} → todo (review bounce {})", short_task(&p.tasks[idx].id), p.tasks[idx].bounces);
        trace(p, now_ms, "task", label, ctx);
    }
}

/// Fold a per-task hygiene grade into the project's rolling clean-build score (item 3). The rolling
/// score is the running AVERAGE of every pass's `hygiene:` grade; when it drops below
/// `crd_pass_grade` a "rolling score sagging: NN" trace fires so the drift is visible before the
/// final audit.
fn accumulate_hygiene(p: &mut Project, grade: u32, now_ms: u64, ctx: &mut Ctx) {
    p.hygiene_sum = p.hygiene_sum.saturating_add(grade as u64);
    p.hygiene_count = p.hygiene_count.saturating_add(1);
    let avg = (p.hygiene_sum / p.hygiene_count as u64) as u32;
    trace(p, now_ms, "hygiene", format!("merge hygiene {grade} — rolling {avg}"), ctx);
    if avg < p.config.crd_pass_grade {
        trace(p, now_ms, "hygiene", format!("rolling score sagging: {avg}"), ctx);
    }
}

/// The `(repo, desk, branch)` a task's worktree git ops need (item 1): the delivery path is the
/// repo, `Task.desk` the worktree, `task/<slug>` the branch. `None` if either path is missing.
fn desk_git_paths(p: &Project, idx: usize) -> Option<(PathBuf, PathBuf, String)> {
    let repo = p.delivery_path.clone()?;
    let desk = p.tasks[idx].desk.clone()?;
    let branch = task_branch(&p.tasks[idx].id.0);
    Some((repo, desk, branch))
}

/// Emit a `RemoveDesk` for a task's worktree when appropriate (item 1): worktree mode, `keep_desks`
/// off, and a desk path is recorded. A no-op otherwise (legacy desks, or the user asked to keep
/// them for inspection).
fn maybe_remove_worktree(p: &mut Project, idx: usize, ctx: &mut Ctx) {
    if !p.worktree_desks || p.config.keep_desks {
        return;
    }
    if let Some((repo, desk, branch)) = desk_git_paths(p, idx) {
        ctx.fx.push(Effect::RemoveDesk { repo, desk, branch });
    }
}

/// A task's worktree merged cleanly into main (item 1): clear the merge gate and complete it.
fn on_desk_merged(p: &mut Project, task: &TaskId, now_ms: u64, ctx: &mut Ctx) {
    let idx = match find_task(p, task) {
        Some(i) => i,
        None => return,
    };
    p.tasks[idx].awaiting_merge = false;
    trace(p, now_ms, "desk", format!("{} merged into main", short_task(task)), ctx);
    complete_passed_task(p, idx, now_ms, ctx);
    ctx.dirty = true;
}

/// A task's worktree merge did not complete (item 1): clear the gate and bounce it; a retry
/// rebranches off the now-advanced main. `is_conflict` (item 4) picks the wording: a REAL content
/// conflict tells the user to resolve it, any other merge failure just says the task was re-queued.
fn on_desk_merge_conflict(
    p: &mut Project,
    task: &TaskId,
    summary: String,
    is_conflict: bool,
    now_ms: u64,
    ctx: &mut Ctx,
) {
    let idx = match find_task(p, task) {
        Some(i) => i,
        None => return,
    };
    p.tasks[idx].awaiting_merge = false;
    let attempt = match &p.tasks[idx].state {
        TaskState::Review { attempt, .. } => *attempt,
        // The task should still be in Review{None} (awaiting merge); if not, nothing to bounce.
        _ => return,
    };
    let note = if is_conflict {
        format!("merge conflict — resolve then re-deliver: {summary}")
    } else {
        format!("merge failed ({summary}) — task re-queued")
    };
    let trace_word = if is_conflict { "conflict" } else { "failed" };
    trace(p, now_ms, "desk", format!("{} merge {trace_word} — bouncing", short_task(task)), ctx);
    bounce_task(p, idx, attempt, note, now_ms, ctx);
    ctx.dirty = true;
}

/// A spawn that failed before producing any report. Re-queue; the third consecutive
/// spawn-side failure (no successful spawn in between) parks the task
/// `SpawnFailed` (5.3).
fn on_spawn_failed(p: &mut Project, task: &TaskId, reason: String, now_ms: u64, ctx: &mut Ctx) {
    let idx = match find_task(p, task) {
        Some(i) => i,
        None => return,
    };
    record(&mut p.tasks[idx], now_ms, format!("spawn-failed:{}", reason));
    let attempt = match &p.tasks[idx].state {
        TaskState::OnProgress { attempt, .. } | TaskState::Review { attempt, .. } => *attempt,
        _ => {
            ctx.dirty = true;
            return;
        }
    };
    ctx.dirty = true;

    if spawn_failure_streak(&p.tasks[idx]) >= 3 {
        maybe_remove_worktree(p, idx, ctx);
        p.tasks[idx].state = TaskState::Parked {
            reason: ParkReason::SpawnFailed(reason),
            attempt,
        };
        check_halt(p, now_ms, ctx);
    } else if matches!(p.tasks[idx].state, TaskState::Review { .. }) {
        p.tasks[idx].state = TaskState::Review {
            binding: None,
            attempt,
        };
    } else {
        set_next_attempt(&mut p.tasks[idx], now_ms, attempt);
        p.tasks[idx].state = TaskState::Todo;
    }
}

/// The per-worker runtime ceiling (5.2.4): the only bound on a runaway sub-agent's
/// token burn, since the host cannot cap contributed sub-agent steps. Any real
/// binding older than `worker_max_runtime_ms` is force-killed and its task re-queued,
/// independent of any liveness signal.
fn runtime_ceiling(p: &mut Project, now_ms: u64, ctx: &mut Ctx) {
    let ceiling = p.config.worker_max_runtime_ms;
    let mut expired: Vec<(usize, u64)> = Vec::new();
    for (i, t) in p.tasks.iter().enumerate() {
        let binding = match &t.state {
            TaskState::OnProgress { binding, .. } => Some(binding),
            TaskState::Review {
                binding: Some(b), ..
            } => Some(b),
            _ => None,
        };
        if let Some(b) = binding {
            if b.ext_agent_id != PROVISIONAL && now_ms.saturating_sub(b.spawned_at_ms) > ceiling {
                expired.push((i, b.ext_agent_id));
            }
        }
    }
    for (i, agent_id) in expired {
        ctx.fx.push(Effect::Kill {
            ext_agent_id: agent_id,
        });
        requeue_failed(p, i, now_ms, "runtime-ceiling", ctx);
    }

    // The project-level research binding (6.2b) shares the same ceiling. An over-age
    // researcher is force-killed and Drafting degrades to a PRD-only TRD — a hung researcher
    // never wedges the pipeline (reconcile killed-path coverage).
    if let Some(b) = &p.research {
        if b.ext_agent_id != PROVISIONAL && now_ms.saturating_sub(b.spawned_at_ms) > ceiling {
            let agent_id = b.ext_agent_id;
            ctx.fx.push(Effect::Kill { ext_agent_id: agent_id });
            research_degrade(p, "runtime ceiling".to_string(), now_ms, ctx);
        }
    }

    // The project-level audit binding (6.2c) shares the same ceiling. An over-age auditor is
    // force-killed and the project degrades to Done WITHOUT an audit — a hung auditor never
    // wedges completion.
    if let Some(b) = &p.audit {
        if b.ext_agent_id != PROVISIONAL && now_ms.saturating_sub(b.spawned_at_ms) > ceiling {
            let agent_id = b.ext_agent_id;
            ctx.fx.push(Effect::Kill { ext_agent_id: agent_id });
            audit_degrade(p, "runtime ceiling".to_string(), now_ms, ctx);
        }
    }
}

// ---------------------------------------------------------------------------
// Dispatch
// ---------------------------------------------------------------------------

/// The deterministic dispatch scan. Reviewers get slot priority so the line drains,
/// then ready workers. Bounded by the session-global `session_capacity` (hard cap)
/// and the per-project `max_workers` soft sub-ceiling.
fn dispatch(p: &mut Project, now_ms: u64, session_capacity: u32, ctx: &mut Ctx) {
    let bound = match &p.bound_session {
        Some(s) => s.clone(),
        None => return,
    };
    let delivery = match &p.delivery_path {
        Some(d) => d.clone(),
        None => return,
    };

    let mut budget = session_capacity;
    if budget == 0 {
        return;
    }
    let max = p.config.max_workers.clamp(1, MAX_PROJECT_WORKERS);
    let mut held = project_in_flight(p);

    for tid in pending_reviews_sorted(p, now_ms) {
        if budget == 0 || held >= max {
            break;
        }
        spawn_reviewer(p, &tid, &bound, &delivery, now_ms, ctx);
        held += 1;
        budget -= 1;
    }

    if p.workflow_home.is_none() && p.workspace.is_none() {
        return; // workers need a desk root (the extension home, or the workspace as fallback)
    }
    for tid in ready_set(&p.tasks) {
        if budget == 0 || held >= max {
            break;
        }
        // Instant-death backoff (item 4): skip a task still inside its cooldown; a later Tick /
        // Reconcile re-scans once `now_ms` passes `dispatch_after_ms` (no busy-wait, no thread).
        if p.tasks.iter().any(|t| t.id == tid && t.dispatch_after_ms > now_ms) {
            continue;
        }
        spawn_worker(p, &tid, &bound, &delivery, now_ms, ctx);
        held += 1;
        budget -= 1;
    }
}

/// Build the per-task desk directory (ARCHITECTURE.md 7.1, item 1). The desk lives under the
/// EXTENSION's own workspace (`~/.koma-workflow`, `Project.workflow_home`), never next to the
/// delivery product — the user rule is that the delivery folder receives ONLY the product. `TaskId.0`
/// is the full hierarchical id `<project>/<epic-slug>/<story-slug>/<task-slug>` (see
/// `office::apply_breakdown`); only the final `/`-delimited segment (the task slug) is used, so
/// nested epic/story path segments never leak into the desk tree.
///
/// - `workflow_home` seeded (the norm): `<home>/desks/<project-slug>/<task-slug>/` — clean, and a
///   git worktree in worktree-desks mode.
/// - `workflow_home` absent (pre-feature state files, and unit tests that don't seed it): the
///   HISTORICAL marker path under the user's workspace, preserving legacy behavior.
///
/// `None` only when neither a workflow home nor a workspace is known (a desk cannot be placed).
fn desk_dir(p: &Project, tid: &TaskId) -> Option<PathBuf> {
    let task_slug = tid.0.rsplit('/').next().unwrap_or(&tid.0);
    match &p.workflow_home {
        Some(home) => Some(home.join("desks").join(&p.id.0).join(task_slug)),
        None => p.workspace.as_ref().map(|ws| {
            ws.join("koma-workflow")
                .join("desks")
                .join(&p.id.0)
                .join(format!("{}--koma-workflow-desk", task_slug))
        }),
    }
}

/// The git branch a task's worktree lives on (item 1): `task/<task-slug>`. The slug is the final
/// `/`-segment of the hierarchical task id (globally unique per `apply_breakdown`) and is
/// `[a-z0-9-]`, so it is always a valid ref name. `pub` so the driver's `git worktree add` uses the
/// exact same name the kernel stamps on its `MergeDesk`/`RemoveDesk` effects (one source of truth).
pub fn task_branch(tid: &str) -> String {
    let slug = tid.rsplit('/').next().unwrap_or(tid);
    format!("task/{slug}")
}

fn spawn_worker(p: &mut Project, tid: &TaskId, bound: &str, delivery: &Path, now_ms: u64, ctx: &mut Ctx) {
    let idx = match find_task(p, tid) {
        Some(i) => i,
        None => return,
    };
    let desk = match desk_dir(p, tid) {
        Some(d) => d,
        None => return, // no desk root known (no workflow home and no workspace)
    };
    let attempt = next_attempt(&p.tasks[idx]);
    let review_notes = p.tasks[idx].last_review.clone();

    // Fold every unread comment into the prompt; flip Pending -> Delivered (delivery
    // receipt). Already-Delivered comments are re-folded (still unread) but not re-timestamped.
    let mut folded: Vec<Comment> = Vec::new();
    for c in p.tasks[idx].comments.iter_mut() {
        match c.receipt {
            Receipt::Read { .. } => {}
            Receipt::Delivered { .. } => folded.push(c.clone()),
            Receipt::Pending => {
                c.receipt = Receipt::Delivered { at_ms: now_ms };
                folded.push(c.clone());
            }
        }
    }

    let prompt = prompts::worker(
        p,
        &p.tasks[idx],
        &desk,
        delivery,
        attempt,
        review_notes.as_deref(),
        &folded,
    );

    // A stable, id-hashed persona (one of 10) — the same task always draws the same
    // worker across respawns/bounces (persona.rs). Carried both as the spawn's agent id
    // and onto the binding so the office view can label the desk.
    let persona = crate::persona::worker_agent_id(&tid.0);

    // Legacy desks need the driver to `mkdir` the scratch dir; worktree desks are materialized by
    // the driver's `git worktree add` inside `exec_spawn` (fresh each dispatch), so no EnsureDesk.
    if !p.worktree_desks {
        ctx.fx.push(Effect::EnsureDesk {
            task: tid.clone(),
            dir: desk.clone(),
        });
    }
    ctx.fx.push(Effect::Spawn {
        task: tid.clone(),
        prompt,
        agent: persona.clone(),
        model: p.config.worker_model.clone(),
    });

    p.tasks[idx].desk = Some(desk);
    p.tasks[idx].state = TaskState::OnProgress {
        binding: AgentBinding {
            ext_agent_id: PROVISIONAL,
            session: bound.to_string(),
            spawned_at_ms: now_ms,
            kind: AgentKind::Worker,
            persona,
        },
        attempt,
    };
    record(
        &mut p.tasks[idx],
        now_ms,
        format!("dispatch worker attempt {}", attempt),
    );
    let label = format!("{} → worker dispatched (attempt {attempt})", short_task(tid));
    trace(p, now_ms, "task", label, ctx);
    ctx.dirty = true;
}

fn spawn_reviewer(p: &mut Project, tid: &TaskId, bound: &str, delivery: &Path, now_ms: u64, ctx: &mut Ctx) {
    let idx = match find_task(p, tid) {
        Some(i) => i,
        None => return,
    };
    let attempt = match &p.tasks[idx].state {
        TaskState::Review { attempt, .. } => *attempt,
        _ => return,
    };
    let rep = report::parse_report(p.tasks[idx].last_report.as_deref().unwrap_or(""));
    let summary = rep.summary.unwrap_or_default();
    let prompt = prompts::reviewer(p, &p.tasks[idx], delivery, &summary, &rep.delivered);

    ctx.fx.push(Effect::Spawn {
        task: tid.clone(),
        prompt,
        agent: "office-reviewer".to_string(),
        model: p.config.reviewer_model.clone(),
    });

    p.tasks[idx].state = TaskState::Review {
        binding: Some(AgentBinding {
            ext_agent_id: PROVISIONAL,
            session: bound.to_string(),
            spawned_at_ms: now_ms,
            kind: AgentKind::Reviewer,
            persona: "office-reviewer".to_string(),
        }),
        attempt,
    };
    record(&mut p.tasks[idx], now_ms, "dispatch reviewer");
    let label = format!("{} → reviewer dispatched", short_task(tid));
    trace(p, now_ms, "task", label, ctx);
    ctx.dirty = true;
}

// ---------------------------------------------------------------------------
// Shared helpers
// ---------------------------------------------------------------------------

/// Re-queue a task whose agent died: worker -> Todo (attempt++), reviewer ->
/// Review{None} (respawn on the next scan). Provisional/real binding alike.
fn requeue_failed(p: &mut Project, idx: usize, now_ms: u64, tag: &str, ctx: &mut Ctx) {
    match &p.tasks[idx].state {
        TaskState::OnProgress { attempt, .. } => {
            let attempt = *attempt;
            record(&mut p.tasks[idx], now_ms, tag);
            set_next_attempt(&mut p.tasks[idx], now_ms, attempt + 1);
            p.tasks[idx].state = TaskState::Todo;
            ctx.dirty = true;
        }
        TaskState::Review { attempt, .. } => {
            let attempt = *attempt;
            record(&mut p.tasks[idx], now_ms, tag);
            p.tasks[idx].state = TaskState::Review {
                binding: None,
                attempt,
            };
            ctx.dirty = true;
        }
        _ => {}
    }
}

/// After any park, if the whole line is stuck behind Parked tasks, halt the project
/// and queue a notice (5.3). No-op unless the project is currently Running.
fn check_halt(p: &mut Project, now_ms: u64, ctx: &mut Ctx) {
    if !matches!(p.phase, ProjectPhase::Running) {
        return;
    }
    if let Some(stuck) = graph::line_is_stuck(p) {
        let culprits: Vec<String> = stuck.parked_blockers.iter().map(|id| id.0.clone()).collect();
        let msg = format!(
            "production line halted: parked task(s) {} block everything",
            culprits.join(", ")
        );
        if let Ok(ph) = step_project(&p.phase, ProjectTransition::Halt { reason: msg.clone() }) {
            p.phase = ph;
            trace(p, now_ms, "phase", format!("halted — {}", trace_preview(&msg, 80)), ctx);
        }
        queue_notice(p, now_ms, msg, ctx);
        ctx.dirty = true;
    }
}

/// Every task is Done (6.2c). If the project carries a CRD and no audit is already in flight,
/// spawn the read-only clean-build auditor INSTEAD of completing — the audit gate
/// ([`on_audit_result`]) decides Done vs a remediation round. Otherwise (no CRD) complete
/// normally. No-op unless Running with a non-empty, fully-Done board.
fn maybe_complete_project(p: &mut Project, now_ms: u64, ctx: &mut Ctx) {
    if !matches!(p.phase, ProjectPhase::Running) {
        return;
    }
    if p.tasks.is_empty() || !p.tasks.iter().all(|t| matches!(t.state, TaskState::Done { .. })) {
        return;
    }
    // SDLC patch track (feature: sdlc-triage): no CRD and merge review IS the gate, so SKIP the final
    // clean-build audit, traced. (A pure patch has an empty CRD and would complete without an audit
    // anyway; this makes the skip explicit + testable. A patch that ESCALATED to enhancement has
    // track != "patch", so it still takes the audit path below.)
    if p.track == "patch" {
        complete_project(p, now_ms);
        trace(p, now_ms, "sdlc", "audit skipped (patch track)", ctx);
        trace(p, now_ms, "phase", "project complete — all tasks done", ctx);
        return;
    }
    // A CRD present + no audit already running -> gate completion on a clean-build audit. If the
    // grade was already passing the project would be Done (phase != Running) and we would not be
    // here, so no explicit "already audited" flag is needed.
    if !p.crd_markdown.trim().is_empty() && p.audit.is_none() {
        start_audit(p, now_ms, ctx);
        return;
    }
    complete_project(p, now_ms);
    trace(p, now_ms, "phase", "project complete — all tasks done", ctx);
}

/// Transition Running -> Done (the terminal completion). Pure phase step; the caller owns the
/// dirty flag / notice.
fn complete_project(p: &mut Project, now_ms: u64) {
    if let Ok(ph) = step_project(&p.phase, ProjectTransition::Complete { at_ms: now_ms }) {
        p.phase = ph;
    }
}

// ---------------------------------------------------------------------------
// Clean-build audit gate (6.2c feature B) — Running-only, deterministic, graceful-degrade
// ---------------------------------------------------------------------------

/// Whether `agent_id` is this project's live auditor binding (6.2c). Provisional (id 0) bindings
/// never match a real host event.
fn audit_bound_to(p: &Project, agent_id: u64) -> bool {
    matches!(&p.audit, Some(b) if b.ext_agent_id == agent_id && agent_id != PROVISIONAL)
}

/// Spawn the read-only clean-build auditor (6.2c). Two-phase like the researcher: emit
/// `SpawnAudit` and record a PROVISIONAL project-level binding so the reconcile ceiling sees it;
/// the driver runs the spawn and feeds back `AuditSpawned` (or `AuditFailed`, which degrades to
/// Done). The project stays Running while grading; dispatch is a no-op with every task Done.
fn start_audit(p: &mut Project, now_ms: u64, ctx: &mut Ctx) {
    let delivery = match &p.delivery_path {
        Some(d) => d.clone(),
        None => {
            // A Running project always has a delivery path; guard anyway so a malformed project
            // completes rather than wedging.
            complete_project(p, now_ms);
            return;
        }
    };
    let prompt = prompts::auditor(p, &delivery);
    ctx.fx.push(Effect::SpawnAudit { prompt });
    p.audit = Some(AgentBinding {
        ext_agent_id: PROVISIONAL,
        session: p.bound_session.clone().unwrap_or_default(),
        spawned_at_ms: now_ms,
        kind: AgentKind::Auditor,
        // Project-level fixed staff: the office view keys the auditor corner off this
        // binding's PRESENCE, not a persona label.
        persona: String::new(),
    });
    trace(p, now_ms, "audit", "spawned — clean-build audit", ctx);
    queue_notice(
        p,
        now_ms,
        format!(
            "office[{}]: all tasks done — running the clean-build audit before I mark it complete.",
            p.id.0
        ),
        ctx,
    );
    ctx.dirty = true;
}

/// The driver recorded the real auditor agent id onto the provisional binding (6.2c).
fn on_audit_spawned(p: &mut Project, agent_id: u64, spawned_at_ms: u64, ctx: &mut Ctx) {
    if let Some(b) = &mut p.audit {
        b.ext_agent_id = agent_id;
        b.spawned_at_ms = spawned_at_ms;
        ctx.dirty = true;
    }
}

/// The auditor could not run or died (6.2c) — spawn failure, cross-process, dead auditor, or the
/// runtime ceiling. Degrade gracefully: clear the binding, complete the project WITHOUT an audit,
/// and tell the user. Never wedges completion.
fn audit_degrade(p: &mut Project, reason: String, now_ms: u64, ctx: &mut Ctx) {
    p.audit = None;
    complete_project(p, now_ms);
    trace(p, now_ms, "audit", format!("degraded: {}", trace_preview(&reason, 80)), ctx);
    queue_notice(
        p,
        now_ms,
        format!("office[{}]: audit skipped: {} — project done.", p.id.0, reason),
        ctx,
    );
    ctx.dirty = true;
}

/// The auditor finished (6.2c): parse the OFFICE-AUDIT grade + failures (tolerant), clear the
/// binding, record the grade, and apply the deterministic gate:
///   grade >= crd_pass_grade            -> Done + notice.
///   grade <  threshold, rounds 1-2     -> one Todo remediation task (high priority, no deps).
///   grade <  threshold, after 2 rounds -> a PARKED remediation task (halt machinery takes over).
/// A missing/unparseable grade FAILS OPEN (Done + notice) — never punishing a formatting slip.
fn on_audit_result(p: &mut Project, text: String, now_ms: u64, ctx: &mut Ctx) {
    p.audit = None;
    let report = report::parse_audit(&text);
    let grade = match report.grade {
        Some(g) => g,
        None => {
            complete_project(p, now_ms);
            trace(p, now_ms, "audit", "inconclusive (no grade) — completing", ctx);
            queue_notice(
                p,
                now_ms,
                format!(
                    "office[{}]: audit inconclusive (no grade reported) — project done.",
                    p.id.0
                ),
                ctx,
            );
            ctx.dirty = true;
            return;
        }
    };
    p.last_audit_grade = Some(grade);

    if grade >= p.config.crd_pass_grade {
        complete_project(p, now_ms);
        trace(p, now_ms, "audit", format!("passed — grade {grade}"), ctx);
        queue_notice(
            p,
            now_ms,
            format!("office[{}]: audit passed: grade {} — project done.", p.id.0, grade),
            ctx,
        );
        ctx.dirty = true;
        return;
    }

    // Sub-threshold. The first two failing audits open an actionable remediation task; a third
    // parks it for the user. `audit_rounds` is checked BEFORE the increment so the literal
    // "audit_rounds < 2 -> Todo round R" ladder from the spec is preserved and survives a reload.
    let failures = clip_failures(&report.failures);
    if p.audit_rounds < 2 {
        p.audit_rounds += 1;
        let round = p.audit_rounds;
        add_remediation_task(p, round, &report.failures, false, now_ms);
        trace(p, now_ms, "audit", format!("grade {} < {} — remediation round {}", grade, p.config.crd_pass_grade, round), ctx);
        queue_notice(
            p,
            now_ms,
            format!(
                "office[{}]: audit grade {} < {} — opened CRD remediation round {}: {}",
                p.id.0, grade, p.config.crd_pass_grade, round, failures
            ),
            ctx,
        );
    } else {
        add_remediation_task(p, p.audit_rounds + 1, &report.failures, true, now_ms);
        trace(p, now_ms, "audit", format!("still failing (grade {grade}) — parked remediation"), ctx);
        queue_notice(
            p,
            now_ms,
            format!(
                "office[{}]: audit still failing after 2 rounds (grade {}): {} — fix manually and unpark, or lower crd_pass_grade in settings.",
                p.id.0, grade, failures
            ),
            ctx,
        );
        check_halt(p, now_ms, ctx);
    }
    ctx.dirty = true;
}

/// Create a CRD remediation task (6.2c). `parked=false` -> Todo (high priority, no deps) for an
/// automated round; `parked=true` -> Parked(AuditFailed) once the automated rounds are exhausted,
/// so the existing halt machinery takes over. The task id carries the round so re-audits never
/// collide.
fn add_remediation_task(p: &mut Project, round: u32, failures: &[String], parked: bool, now_ms: u64) {
    let id = TaskId(format!("{}/crd-remediation-round-{}", p.id.0, round));
    let description = if failures.is_empty() {
        "The clean-build audit graded the delivery below the pass threshold. Bring the delivery \
into full compliance with the Clean-build Requirement Document (docs tab)."
            .to_string()
    } else {
        format!(
            "The clean-build audit graded the delivery below the pass threshold. Fix these failing CRD items:\n- {}",
            failures.join("\n- ")
        )
    };
    let state = if parked {
        TaskState::Parked {
            reason: ParkReason::AuditFailed(clip_failures(failures)),
            attempt: 1,
        }
    } else {
        TaskState::Todo
    };
    let mut task = Task {
        id,
        title: format!("CRD remediation round {}", round),
        description,
        acceptance: vec![
            "Every failing CRD item from the audit is resolved".to_string(),
            "The delivery satisfies the Clean-build Requirement Document".to_string(),
        ],
        blocked_by: Vec::new(),
        priority: 100,
        state,
        bounces: 0,
        comments: Vec::new(),
        desk: None,
        last_report: None,
        last_review: None,
        history: Vec::new(),
        diff_stat: None,
        awaiting_merge: false,
        dispatch_after_ms: 0,
    };
    record(&mut task, now_ms, format!("crd-remediation:round-{}", round));
    p.tasks.push(task);
}

/// Clip an audit failure list to a short, single-line preview for a chat notice.
fn clip_failures(failures: &[String]) -> String {
    if failures.is_empty() {
        return "see the CRD checklist".to_string();
    }
    const MAX_ITEMS: usize = 3;
    const MAX_LEN: usize = 300;
    let mut preview: Vec<String> = failures.iter().take(MAX_ITEMS).cloned().collect();
    if failures.len() > MAX_ITEMS {
        preview.push(format!("(+{} more)", failures.len() - MAX_ITEMS));
    }
    let joined = preview.join("; ");
    if joined.len() <= MAX_LEN {
        return joined;
    }
    let mut cut = MAX_LEN;
    while !joined.is_char_boundary(cut) {
        cut -= 1;
    }
    format!("{}…", &joined[..cut])
}

/// Clip a persona reply to fit an outbox notice (driver sends <=4KB per tick; the
/// host chat.prompt cap is 16KB). Cuts on a char boundary and marks the clip.
fn clip_notice(reply: &str) -> String {
    const MAX: usize = 3200;
    if reply.len() <= MAX {
        return reply.to_string();
    }
    let mut cut = MAX;
    while !reply.is_char_boundary(cut) {
        cut -= 1;
    }
    format!("{}\n[clipped — full reply in the Workflow panel]", &reply[..cut])
}

/// Mint an outbox notice and emit the chat.prompt effect for it.
fn queue_notice(p: &mut Project, _now_ms: u64, text: String, ctx: &mut Ctx) {
    let id = p.outbox.iter().map(|n| n.id).max().unwrap_or(0) + 1;
    p.outbox.push(crate::domain::OutboundNotice {
        id,
        text: text.clone(),
        sent: false,
        paused: false,
    });
    ctx.fx.push(Effect::QueueChatPrompt {
        notice_id: id,
        text,
    });
}

/// The machine-diary ring cap (feature: tracelog). Newest-last; the oldest entries drop once the
/// ring exceeds this, so a long-running project can never balloon `state.json`.
const TRACE_CAP: usize = 200;

/// Append a machine-diary trace event (feature: tracelog) — what the office machine just DID, one
/// line, never document content. Every trace is a persisted state change, so it marks the tick
/// dirty (flushing the trailing `Persist` + `PanelPush` that carries the ring to the panel). The
/// ring is capped at [`TRACE_CAP`] with the oldest entries dropped.
fn trace(p: &mut Project, now_ms: u64, kind: &str, summary: impl Into<String>, ctx: &mut Ctx) {
    p.trace.push(TraceEvent {
        ts: now_ms as i64,
        kind: kind.to_string(),
        summary: summary.into(),
    });
    let len = p.trace.len();
    if len > TRACE_CAP {
        p.trace.drain(0..len - TRACE_CAP);
    }
    ctx.dirty = true;
}

/// Clip free text to a short, single-line trace preview (feature: tracelog): collapse whitespace
/// runs to single spaces, then truncate to `max` characters with an ellipsis. Char-count based,
/// so it never splits a UTF-8 boundary and never leaks a multi-line document body into a summary.
fn trace_preview(s: &str, max: usize) -> String {
    let flat = s.split_whitespace().collect::<Vec<_>>().join(" ");
    if flat.chars().count() <= max {
        return flat;
    }
    let clipped: String = flat.chars().take(max).collect();
    format!("{clipped}…")
}

/// Lowercase phase label for trace summaries (feature: tracelog).
fn phase_label(phase: &ProjectPhase) -> &'static str {
    match phase {
        ProjectPhase::Drafting => "drafting",
        ProjectPhase::Ready => "ready",
        ProjectPhase::Running => "running",
        ProjectPhase::Interrupted => "interrupted",
        ProjectPhase::Halted { .. } => "halted",
        ProjectPhase::Done { .. } => "done",
    }
}

/// The short (last-segment) task slug for a trace summary (feature: tracelog): a `TaskId` is the
/// full hierarchical `<project>/<epic>/<story>/<task>`, so the final segment reads cleanly in a
/// one-line diary entry without the nested path noise.
fn short_task(tid: &TaskId) -> &str {
    tid.0.rsplit('/').next().unwrap_or(&tid.0)
}

/// Flip acked comments Delivered -> Read. A comment still Pending (never delivered)
/// is NOT flipped — the office never claims an agent read what it never received.
fn apply_acks(t: &mut Task, ids: &[CommentId], now_ms: u64) {
    for id in ids {
        if let Some(c) = t.comments.iter_mut().find(|c| &c.id == id) {
            if matches!(c.receipt, Receipt::Delivered { .. }) {
                c.receipt = Receipt::Read { at_ms: now_ms };
            }
        }
    }
}

/// The real (non-provisional) ext agent id a comment can be pushed to mid-run via `agents.send`
/// (feature 4), if this state carries a LIVE binding: an in-flight worker (`OnProgress`) or a
/// spawned reviewer (`Review` with a reviewer binding). A provisional binding (id `PROVISIONAL`,
/// spawn not yet acked) or any bindingless state yields `None` — the comment then waits `Pending`
/// for the spawn-boundary fold to deliver on the next spawn.
fn live_binding_id(state: &TaskState) -> Option<u64> {
    match state {
        TaskState::OnProgress { binding, .. } if binding.ext_agent_id != PROVISIONAL => {
            Some(binding.ext_agent_id)
        }
        TaskState::Review { binding: Some(b), .. } if b.ext_agent_id != PROVISIONAL => {
            Some(b.ext_agent_id)
        }
        _ => None,
    }
}

/// Apply a `CommentDelivered` host event (feature 4): the driver's `agents.send` for this comment
/// succeeded, so flip its receipt `Pending -> Delivered`. Only from `Pending` — a comment already
/// `Read` (acked) or `Delivered` is left untouched (never downgrade a read receipt, never
/// re-timestamp). An unknown task/comment id is a silent no-op.
fn on_comment_delivered(
    p: &mut Project,
    task: &TaskId,
    comment_id: CommentId,
    now_ms: u64,
    ctx: &mut Ctx,
) {
    if let Some(idx) = find_task(p, task) {
        if let Some(c) = p.tasks[idx].comments.iter_mut().find(|c| c.id == comment_id) {
            if matches!(c.receipt, Receipt::Pending) {
                c.receipt = Receipt::Delivered { at_ms: now_ms };
                ctx.dirty = true;
            }
        }
    }
}

fn project_in_flight(p: &Project) -> u32 {
    p.tasks
        .iter()
        .filter(|t| {
            matches!(
                t.state,
                TaskState::OnProgress { .. } | TaskState::Review { binding: Some(_), .. }
            )
        })
        .count() as u32
}

fn pending_reviews_sorted(p: &Project, now_ms: u64) -> Vec<TaskId> {
    let mut v: Vec<&Task> = p
        .tasks
        .iter()
        .filter(|t| matches!(t.state, TaskState::Review { binding: None, .. }))
        // Not while a merge is in flight for this task (item 1: the reviewer already passed).
        .filter(|t| !t.awaiting_merge)
        // Instant-death backoff (item 4): honor a reviewer cooldown just like a worker one.
        .filter(|t| t.dispatch_after_ms <= now_ms)
        // Worktree desks (item 2): a review only starts once the worker's tree has been committed
        // onto its branch (the diff-stat is stashed then). Legacy desks have no such gate.
        .filter(|t| !p.worktree_desks || t.diff_stat.is_some())
        .collect();
    v.sort_by(|a, b| b.priority.cmp(&a.priority).then_with(|| a.id.cmp(&b.id)));
    v.into_iter().map(|t| t.id.clone()).collect()
}

fn binding_kind(state: &TaskState) -> Option<AgentKind> {
    match state {
        TaskState::OnProgress { binding, .. } => Some(binding.kind),
        TaskState::Review {
            binding: Some(b), ..
        } => Some(b.kind),
        _ => None,
    }
}

fn find_task(p: &Project, id: &TaskId) -> Option<usize> {
    p.tasks.iter().position(|t| &t.id == id)
}

fn find_by_agent(p: &Project, agent_id: u64) -> Option<usize> {
    p.tasks.iter().position(|t| match &t.state {
        TaskState::OnProgress { binding, .. } => binding.ext_agent_id == agent_id,
        TaskState::Review {
            binding: Some(b), ..
        } => b.ext_agent_id == agent_id,
        _ => false,
    })
}

fn record(t: &mut Task, now_ms: u64, event: impl Into<String>) {
    t.history.push(TaskEvent {
        at_ms: now_ms,
        event: event.into(),
    });
}

/// The attempt number the NEXT dispatch of this task should use, read from the
/// `next-attempt:<n>` ledger marker (written whenever the task is re-queued). Fresh
/// tasks with no marker start at attempt 1.
fn next_attempt(t: &Task) -> u32 {
    t.history
        .iter()
        .rev()
        .find_map(|e| {
            e.event
                .strip_prefix("next-attempt:")
                .and_then(|s| s.trim().parse::<u32>().ok())
        })
        .unwrap_or(1)
}

fn set_next_attempt(t: &mut Task, now_ms: u64, n: u32) {
    record(t, now_ms, format!("next-attempt:{}", n));
}

/// Count of spawn-side failures since the last successful spawn (a `spawned:` event).
/// Three in a row with no successful spawn between them escalates to SpawnFailed.
fn spawn_failure_streak(t: &Task) -> u32 {
    let mut count = 0;
    for e in t.history.iter().rev() {
        if e.event.starts_with("spawn-failed") {
            count += 1;
        } else if e.event.starts_with("spawned") {
            break;
        }
    }
    count
}
