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
//! ## Drafting pipeline + resume rule (6.2b / 6.2c)
//! Drafting is a linear pipeline: `PRD -> [gate] -> research -> TRD -> [gate] -> CRD -> [gate] ->
//! breakdown -> Ready`. Each `[gate]` is the safeguard no-assume check (feature C): after a
//! ```prd/```trd/```crd fence is captured, the kernel emits an `AssumeCheck{Prd,Trd,Crd}` invoke
//! whose CLEAN verdict proceeds to that doc's successor stage (PRD->research, TRD->CRD,
//! CRD->breakdown), and whose ASSUMPTIONS verdict STOPS the pipeline by storing
//! `Project.pending_assumptions` and noticing the user.
//!
//! **No hidden "which stage is deferred" state is kept.** The successor stage is a pure function
//! of the doc identity carried on the check's `InvokePurpose`, so it is always recomputable and
//! survives a store reload. `pending_assumptions` (persisted) records only that the LAST gate
//! found ungrounded assumptions; ANY subsequent clean check clears it. A stopped gate is
//! re-entered the only way a doc is ever captured — the user answers in chat, the persona
//! re-emits the fence, and the fresh capture re-runs the gate. If a reload lands mid-invoke the
//! in-flight check is simply lost (like any invoke) and the next `OfficeMessage` re-runs it. So
//! the resume point is reconstructible from pure state: `phase` + which docs are non-empty +
//! `pending_assumptions` + `assumption_check`.
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
    Project, ProjectPhase, Receipt, Task, TaskEvent, TaskId, TaskState,
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
    /// verdict; a false verdict never transitions (the hard gate).
    Authorize {
        delivery_path: PathBuf,
        allow_outside_workspace: bool,
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
    },
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
    /// A sub-agent reached a terminal host status (`done`/`error`/`killed`).
    AgentsDone { agent_id: u64, status: String },
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
                soft_interrupt(p, ctx);
            }
        }
        Command::Resume => {
            if let Ok(ph) = step_project(&p.phase, ProjectTransition::Resume) {
                p.phase = ph;
                ctx.dirty = true;
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
        Command::OfficeMessage { text } => office_message(p, text, ctx),
        Command::RequestBreakdown => request_breakdown(p, ctx),
        Command::Authorize {
            delivery_path,
            allow_outside_workspace,
        } => authorize(p, delivery_path, allow_outside_workspace, now_ms, ctx),
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
        }
    }
}

// ---------------------------------------------------------------------------
// Front office (6.2 / 6.3) — off-loop invoke choreography
// ---------------------------------------------------------------------------

/// Append the user turn and issue a persona invoke. If the assembled prompt would cross
/// the fold threshold, a summarize invoke is issued FIRST (6.2); the persona invoke is
/// re-issued from `invoke_result` once the fold lands.
fn office_message(p: &mut Project, text: String, ctx: &mut Ctx) {
    p.office_transcript.push(ChatMsg {
        who: ChatAuthor::User,
        text,
    });
    ctx.dirty = true;

    if office::should_fold(p, "") {
        let (system, prompt) = office::build_fold(p);
        emit_invoke(ctx, InvokePurpose::Fold, &p.config.office_role, system, prompt);
    } else {
        let (system, prompt) = office::build_invoke(p, "");
        emit_invoke(ctx, InvokePurpose::Persona, &p.config.office_role, system, prompt);
    }
}

/// Issue the breakdown invoke for the current PRD (6.3.2).
fn request_breakdown(p: &mut Project, ctx: &mut Ctx) {
    let (system, prompt) = office::build_breakdown_prompt(p, None, false);
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
}

/// The driver recorded the real research agent id onto the provisional binding (6.2b).
fn on_research_spawned(p: &mut Project, agent_id: u64, spawned_at_ms: u64, ctx: &mut Ctx) {
    if let Some(b) = &mut p.research {
        b.ext_agent_id = agent_id;
        b.spawned_at_ms = spawned_at_ms;
        ctx.dirty = true;
    }
}

/// The researcher finished (6.2b): parse the OFFICE-RESEARCH findings (tolerant; a missing
/// block falls back to the whole reply text), store the capped notes, clear the binding, and
/// draft the TRD.
fn on_research_result(p: &mut Project, text: String, now_ms: u64, ctx: &mut Ctx) {
    p.research_notes = office::extract_research(&text);
    p.research = None;
    queue_notice(
        p,
        now_ms,
        format!("office[{}]: research done — drafting the TRD.", p.id.0),
        ctx,
    );
    start_trd_invoke(p, ctx);
    ctx.dirty = true;
}

/// Research could not run or died — spawn failure, dead researcher, or runtime ceiling (6.2b).
/// Degrade gracefully: clear the binding, tell the user, and draft the TRD from the PRD alone.
/// Never wedges Drafting.
fn research_degrade(p: &mut Project, reason: String, now_ms: u64, ctx: &mut Ctx) {
    p.research = None;
    queue_notice(
        p,
        now_ms,
        format!(
            "office[{}]: research skipped: {}; drafting the TRD from the PRD alone.",
            p.id.0, reason
        ),
        ctx,
    );
    start_trd_invoke(p, ctx);
    ctx.dirty = true;
}

/// Issue the TRD authoring invoke (6.2b): PRD (+ research notes when present) -> a ```trd
/// fenced markdown document. Off-loop like every other invoke.
fn start_trd_invoke(p: &mut Project, ctx: &mut Ctx) {
    let (system, prompt) = office::build_trd_prompt(p);
    emit_invoke(ctx, InvokePurpose::Trd, &p.config.office_role, system, prompt);
}

/// The TRD invoke returned (6.2b). `Ok` with a ```trd fence -> store it and run the safeguard
/// gate (feature C) whose clean verdict proceeds to the CRD (feature A). A missing fence, or any
/// `Err` (e.g. a second timeout after the driver's one pool-level retry), still proceeds to the
/// CRD invoke — from whatever docs exist — so Drafting never wedges on a TRD failure.
fn handle_trd_result(p: &mut Project, outcome: Result<String, String>, now_ms: u64, ctx: &mut Ctx) {
    match outcome {
        Ok(text) => match office::extract_fenced(&text, "trd") {
            Some(trd) => {
                p.trd_markdown = trd;
                queue_notice(
                    p,
                    now_ms,
                    format!(
                        "office[{}]: TRD drafted (panel) — checking assumptions before the clean-build requirements.",
                        p.id.0
                    ),
                    ctx,
                );
                let body = p.trd_markdown.clone();
                gate_doc(p, InvokePurpose::AssumeCheckTrd, "TRD", &body, Deferred::Crd, now_ms, ctx);
                ctx.dirty = true;
                return;
            }
            None => queue_notice(
                p,
                now_ms,
                format!(
                    "office[{}]: TRD draft arrived without a fenced block; drafting the clean-build requirements from the PRD.",
                    p.id.0
                ),
                ctx,
            ),
        },
        Err(e) => queue_notice(
            p,
            now_ms,
            format!(
                "office[{}]: TRD call failed: {}; drafting the clean-build requirements from the PRD alone.",
                p.id.0, e
            ),
            ctx,
        ),
    }
    // No TRD captured -> nothing to safeguard-check; proceed straight to the CRD invoke.
    start_crd_invoke(p, ctx);
    ctx.dirty = true;
}

/// Issue the CRD authoring invoke (6.2c): PRD (+ TRD when present) -> a ```crd fenced Clean-build
/// Requirement Document. Off-loop, on the office role, like the TRD invoke.
fn start_crd_invoke(p: &mut Project, ctx: &mut Ctx) {
    let (system, prompt) = office::build_crd_prompt(p);
    emit_invoke(ctx, InvokePurpose::Crd, &p.config.office_role, system, prompt);
}

/// The CRD invoke returned (6.2c). `Ok` with a ```crd fence -> store it and run the safeguard
/// gate whose clean verdict requests the breakdown. A missing fence or any `Err` STILL requests
/// the breakdown — the project simply completes without a clean-build audit (never wedges).
fn handle_crd_result(p: &mut Project, outcome: Result<String, String>, now_ms: u64, ctx: &mut Ctx) {
    match outcome {
        Ok(text) => match office::extract_fenced(&text, "crd") {
            Some(crd) => {
                p.crd_markdown = crd;
                queue_notice(
                    p,
                    now_ms,
                    format!(
                        "office[{}]: clean-build requirements drafted (panel) — checking assumptions before the breakdown.",
                        p.id.0
                    ),
                    ctx,
                );
                let body = p.crd_markdown.clone();
                gate_doc(p, InvokePurpose::AssumeCheckCrd, "CRD", &body, Deferred::Breakdown, now_ms, ctx);
                ctx.dirty = true;
                return;
            }
            None => queue_notice(
                p,
                now_ms,
                format!(
                    "office[{}]: CRD call skipped (no fenced block); the project will complete without a clean-build audit.",
                    p.id.0
                ),
                ctx,
            ),
        },
        Err(e) => queue_notice(
            p,
            now_ms,
            format!(
                "office[{}]: CRD call failed: {}; the project will complete without a clean-build audit.",
                p.id.0, e
            ),
            ctx,
        ),
    }
    // No CRD captured -> nothing to safeguard-check and no audit later; break down anyway.
    request_breakdown(p, ctx);
    ctx.dirty = true;
}

// ---------------------------------------------------------------------------
// Safeguard no-assume gate (6.2c feature C) — Drafting doc captures
// ---------------------------------------------------------------------------

/// The pipeline stage a captured drafting doc proceeds to once its safeguard gate is clean. The
/// gate is stateless: the AssumeCheck result's purpose (`AssumeCheck{Prd,Trd,Crd}`) names the
/// doc, and this maps doc -> successor stage deterministically, so which stage was deferred is
/// always recomputable and never needs persisting (kernel.rs pipeline resume rule).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Deferred {
    Research,
    Crd,
    Breakdown,
}

/// Run the pipeline stage a doc capture was gating (6.2c). PRD -> research, TRD -> CRD, CRD ->
/// breakdown.
fn run_deferred(p: &mut Project, deferred: Deferred, now_ms: u64, ctx: &mut Ctx) {
    match deferred {
        Deferred::Research => start_research(p, now_ms, ctx),
        Deferred::Crd => start_crd_invoke(p, ctx),
        Deferred::Breakdown => request_breakdown(p, ctx),
    }
}

/// Gate a freshly-captured drafting doc through the safeguard no-assume check (6.2c). When the
/// gate is disabled (`config.assumption_check == false`) it is a straight pass-through to the
/// deferred stage. Otherwise it emits an `AssumeCheck*` invoke on the `safeguard_role`; the
/// result (in [`handle_assume_check_result`]) either proceeds to `deferred` (clean / fail-open)
/// or stops the pipeline with `pending_assumptions`.
fn gate_doc(
    p: &mut Project,
    purpose: InvokePurpose,
    label: &str,
    body: &str,
    deferred: Deferred,
    now_ms: u64,
    ctx: &mut Ctx,
) {
    if !p.config.assumption_check {
        run_deferred(p, deferred, now_ms, ctx);
        return;
    }
    let (system, prompt) = office::build_assume_check_prompt(p, label, body);
    emit_invoke(ctx, purpose, &p.config.safeguard_role, system, prompt);
}

/// A safeguard assumption-check returned (6.2c). `clean` (or an unparseable block) clears
/// `pending_assumptions` and proceeds to the deferred stage; `assumptions` STOPS the pipeline,
/// storing the flagged items and noticing the user (the doc is stored/visible either way, and a
/// subsequent chat + fresh fence re-runs this gate). `Err` FAILS OPEN: proceed with a notice —
/// a flaky safeguard must never wedge Drafting.
fn handle_assume_check_result(
    p: &mut Project,
    deferred: Deferred,
    doc_label: &str,
    outcome: Result<String, String>,
    now_ms: u64,
    ctx: &mut Ctx,
) {
    match outcome {
        Err(e) => {
            queue_notice(
                p,
                now_ms,
                format!("office[{}]: assumption check skipped: {}; continuing.", p.id.0, e),
                ctx,
            );
            p.pending_assumptions.clear();
            run_deferred(p, deferred, now_ms, ctx);
        }
        Ok(text) => match report::parse_assume_check(&text) {
            Some(check)
                if check.verdict == report::AssumeVerdict::Assumptions && !check.items.is_empty() =>
            {
                let items = clip_assumptions(&check.items);
                let n = check.items.len();
                queue_notice(
                    p,
                    now_ms,
                    format!(
                        "office[{}]: {} drafted but contains {} unapproved assumption{}: {} — approve them, answer in chat, or say 'you decide'.",
                        p.id.0,
                        doc_label,
                        n,
                        if n == 1 { "" } else { "s" },
                        items
                    ),
                    ctx,
                );
                p.pending_assumptions = check.items;
                // STOP: the doc is stored/visible; the user must act before the pipeline proceeds.
            }
            _ => {
                // Clean, or an unparseable/inconclusive block -> fail open and proceed.
                p.pending_assumptions.clear();
                run_deferred(p, deferred, now_ms, ctx);
            }
        },
    }
    ctx.dirty = true;
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
/// before transitioning `Ready -> Running`.
fn authorize(p: &mut Project, delivery_path: PathBuf, allow_outside: bool, now_ms: u64, ctx: &mut Ctx) {
    match office::authorize(p, delivery_path, allow_outside) {
        Ok(()) => ctx.dirty = true,
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
            queue_notice(p, now_ms, notice, ctx);
            ctx.dirty = true;
        }
    }
}

/// Apply an off-loop invoke result (5.1). Purpose-tagged so no persistent per-request
/// bookkeeping is needed — the kernel reacts to the result as an ordinary command.
fn invoke_result(p: &mut Project, purpose: InvokePurpose, outcome: Result<String, String>, now_ms: u64, ctx: &mut Ctx) {
    match purpose {
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
            // Drafting doc captures (6.2 / 6.2b / 6.2c): a ```prd / ```trd / ```crd fence in a
            // Drafting reply IS that doc. Land it, then run the safeguard gate (feature C) whose
            // clean verdict PROCEEDS to that doc's pipeline stage — PRD -> research, TRD -> CRD,
            // CRD -> breakdown. Without capture the persona chats forever while the board stays
            // empty (live-test 2026-07-15). A re-emitted fence after a stopped gate re-runs the
            // check (kernel.rs pipeline resume rule).
            if matches!(p.phase, ProjectPhase::Drafting) {
                if let Some(prd) = office::extract_prd(&reply) {
                    p.prd_markdown = prd;
                    queue_notice(
                        p,
                        now_ms,
                        format!(
                            "office[{}]: PRD drafted (full text in the Workflow panel) — checking assumptions before I research the stack. I will report as the board fills in; do not authorize yet.",
                            p.id.0
                        ),
                        ctx,
                    );
                    let body = p.prd_markdown.clone();
                    gate_doc(p, InvokePurpose::AssumeCheckPrd, "PRD", &body, Deferred::Research, now_ms, ctx);
                    ctx.dirty = true;
                    return;
                }
                if let Some(trd) = office::extract_fenced(&reply, "trd") {
                    p.trd_markdown = trd;
                    queue_notice(p, now_ms, format!("office[{}]: TRD updated (panel).", p.id.0), ctx);
                    let body = p.trd_markdown.clone();
                    gate_doc(p, InvokePurpose::AssumeCheckTrd, "TRD", &body, Deferred::Crd, now_ms, ctx);
                    ctx.dirty = true;
                    return;
                }
                if let Some(crd) = office::extract_fenced(&reply, "crd") {
                    p.crd_markdown = crd;
                    queue_notice(p, now_ms, format!("office[{}]: CRD updated (panel).", p.id.0), ctx);
                    let body = p.crd_markdown.clone();
                    gate_doc(p, InvokePurpose::AssumeCheckCrd, "CRD", &body, Deferred::Breakdown, now_ms, ctx);
                    ctx.dirty = true;
                    return;
                }
            }

            // A chat-authored ```trd in Ready is a RE-PLAN trigger, not a pipeline stage (6.2b):
            // capture it and point at workflow_breakdown. It does NOT run the gate or auto-run
            // CRD/breakdown — the deterministic pipeline is what drives those.
            if matches!(p.phase, ProjectPhase::Ready) {
                if let Some(trd) = office::extract_fenced(&reply, "trd") {
                    p.trd_markdown = trd;
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
            emit_invoke(ctx, InvokePurpose::Persona, &p.config.office_role, system, prompt);
        }
        InvokePurpose::Breakdown => handle_breakdown_result(p, outcome, now_ms, ctx),
        InvokePurpose::BreakdownReask => handle_breakdown_reask_result(p, outcome, now_ms, ctx),
        InvokePurpose::BreakdownCompact => handle_breakdown_compact_result(p, outcome, now_ms, ctx),
        InvokePurpose::Trd => handle_trd_result(p, outcome, now_ms, ctx),
        InvokePurpose::Crd => handle_crd_result(p, outcome, now_ms, ctx),
        InvokePurpose::AssumeCheckPrd => {
            handle_assume_check_result(p, Deferred::Research, "PRD", outcome, now_ms, ctx)
        }
        InvokePurpose::AssumeCheckTrd => {
            handle_assume_check_result(p, Deferred::Crd, "TRD", outcome, now_ms, ctx)
        }
        InvokePurpose::AssumeCheckCrd => {
            handle_assume_check_result(p, Deferred::Breakdown, "CRD", outcome, now_ms, ctx)
        }
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
        Ok(breakdown) => land_breakdown(p, breakdown, now_ms, ctx),
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
        Ok(breakdown) => land_breakdown(p, breakdown, now_ms, ctx),
        Err(e) => {
            queue_notice(
                p,
                now_ms,
                format!("office breakdown rejected twice ({e:?}); edit the board manually"),
                ctx,
            );
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
        Ok(breakdown) => land_breakdown(p, breakdown, now_ms, ctx),
        Err(e) => surface_compact_breakdown_failure(
            p,
            now_ms,
            format!("office breakdown (compact retry) rejected: {e:?}"),
            ctx,
        ),
    }
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

/// Emit an `InvokeModel` effect. `req_id` is a placeholder (0) — the driver mints the
/// real id when it hands the job to the off-loop invoke pool (5.1); the kernel matches
/// results by `purpose`, not id.
fn emit_invoke(ctx: &mut Ctx, purpose: InvokePurpose, role: &str, system: String, prompt: String) {
    ctx.fx.push(Effect::InvokeModel {
        req_id: 0,
        purpose,
        role: role.to_string(),
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
        InvokePurpose::Persona
        | InvokePurpose::Trd
        | InvokePurpose::Crd
        | InvokePurpose::Fold
        | InvokePurpose::AssumeCheckPrd
        | InvokePurpose::AssumeCheckTrd
        | InvokePurpose::AssumeCheckCrd => None,
    }
}

/// Hard interrupt (default): stop dispatch, kill every tracked binding, normalize
/// in-flight tasks. Workers -> Todo (attempt preserved, not a bounce); reviewers ->
/// Review{None} (reviewer respawns on resume). Desks are retained (5.5).
fn hard_interrupt(p: &mut Project, now_ms: u64, ctx: &mut Ctx) {
    if let Ok(ph) = step_project(&p.phase, ProjectTransition::Interrupt) {
        p.phase = ph;
    }
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
/// keep flowing so running agents finish and their results are processed (5.5).
fn soft_interrupt(p: &mut Project, ctx: &mut Ctx) {
    if let Ok(ph) = step_project(&p.phase, ProjectTransition::Interrupt) {
        p.phase = ph;
        ctx.dirty = true;
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
        HostEvent::AgentsDone { agent_id, status } => on_agents_done(p, agent_id, &status, now_ms, ctx),
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

/// Terminal host status. `done` -> fetch the report (no state change yet);
/// `error`/`killed`/anything else -> re-queue the task (worker -> Todo attempt++,
/// reviewer -> Review{None}).
fn on_agents_done(p: &mut Project, agent_id: u64, status: &str, now_ms: u64, ctx: &mut Ctx) {
    // Research binding (6.2b) is project-level, checked before the task bindings. `done` ->
    // fetch the findings (existing FetchResult path); anything else is a dead researcher and
    // degrades exactly like a spawn failure (never wedges Drafting).
    if research_bound_to(p, agent_id) {
        if status.eq_ignore_ascii_case("done") {
            ctx.fx.push(Effect::FetchResult { ext_agent_id: agent_id });
        } else {
            research_degrade(p, format!("researcher {status}"), now_ms, ctx);
        }
        return;
    }
    // The clean-build auditor binding (6.2c) is project-level like research: `done` fetches the
    // OFFICE-AUDIT verdict; anything else is a dead auditor and degrades to Done (never wedges).
    if audit_bound_to(p, agent_id) {
        if status.eq_ignore_ascii_case("done") {
            ctx.fx.push(Effect::FetchResult { ext_agent_id: agent_id });
        } else {
            audit_degrade(p, format!("auditor {status}"), now_ms, ctx);
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
        requeue_failed(p, idx, now_ms, "worker-error", ctx);
    }
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
            ctx.dirty = true;
        }
        ReportStatus::Blocked => {
            let reason = rep.blocked_reason.unwrap_or_default();
            record(&mut p.tasks[idx], now_ms, "report:blocked");
            p.tasks[idx].state = TaskState::Parked {
                reason: ParkReason::WorkerBlocked(reason),
                attempt,
            };
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
            p.tasks[idx].last_review = rev.reasons.or(Some(text));
            record(&mut p.tasks[idx], now_ms, "review:pass");
            p.tasks[idx].state = TaskState::Done { at_ms: now_ms };
            ctx.dirty = true;
            maybe_complete_project(p, now_ms, ctx);
            check_halt(p, now_ms, ctx);
        }
        Verdict::Fail | Verdict::Unparseable => {
            p.tasks[idx].bounces += 1;
            p.tasks[idx].last_review = rev.reasons.or(Some(text));
            record(&mut p.tasks[idx], now_ms, "review:fail");
            ctx.dirty = true;

            if p.tasks[idx].bounces > p.config.bounce_budget {
                let notice = format!(
                    "production line: task {} '{}' exceeded the review bounce budget; the office parked it. Advise or edit the board.",
                    p.tasks[idx].id.0, p.tasks[idx].title
                );
                queue_notice(p, now_ms, notice, ctx);
                p.tasks[idx].state = TaskState::Parked {
                    reason: ParkReason::ReviewBounceBudget,
                    attempt,
                };
                check_halt(p, now_ms, ctx);
            } else {
                set_next_attempt(&mut p.tasks[idx], now_ms, attempt + 1);
                p.tasks[idx].state = TaskState::Todo;
            }
        }
    }
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

    for tid in pending_reviews_sorted(p) {
        if budget == 0 || held >= max {
            break;
        }
        spawn_reviewer(p, &tid, &bound, &delivery, now_ms, ctx);
        held += 1;
        budget -= 1;
    }

    if p.workspace.is_none() {
        return; // workers need a workspace for their desk
    }
    for tid in ready_set(&p.tasks) {
        if budget == 0 || held >= max {
            break;
        }
        spawn_worker(p, &tid, &bound, &delivery, now_ms, ctx);
        held += 1;
        budget -= 1;
    }
}

/// Build the per-task desk directory (ARCHITECTURE.md 7.1): a single flat,
/// human-readable, obviously-marked dir `desks/<project-slug>/<task-slug>--koma-workflow-desk/`.
/// `TaskId.0` is the full hierarchical id `<project>/<epic-slug>/<story-slug>/<task-slug>`
/// (see `office::apply_breakdown`); only the final `/`-delimited segment (the task slug)
/// is used here, so nested epic/story path segments never leak into the desk tree.
fn desk_dir(workspace: &Path, project_slug: &str, tid: &TaskId) -> PathBuf {
    let task_slug = tid.0.rsplit('/').next().unwrap_or(&tid.0);
    workspace
        .join("koma-workflow")
        .join("desks")
        .join(project_slug)
        .join(format!("{}--koma-workflow-desk", task_slug))
}

fn spawn_worker(p: &mut Project, tid: &TaskId, bound: &str, delivery: &Path, now_ms: u64, ctx: &mut Ctx) {
    let idx = match find_task(p, tid) {
        Some(i) => i,
        None => return,
    };
    let workspace = match &p.workspace {
        Some(w) => w.clone(),
        None => return,
    };
    let desk = desk_dir(&workspace, &p.id.0, tid);
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

    ctx.fx.push(Effect::EnsureDesk {
        task: tid.clone(),
        dir: desk.clone(),
    });
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
    // A CRD present + no audit already running -> gate completion on a clean-build audit. If the
    // grade was already passing the project would be Done (phase != Running) and we would not be
    // here, so no explicit "already audited" flag is needed.
    if !p.crd_markdown.trim().is_empty() && p.audit.is_none() {
        start_audit(p, now_ms, ctx);
        return;
    }
    complete_project(p, now_ms);
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

fn pending_reviews_sorted(p: &Project) -> Vec<TaskId> {
    let mut v: Vec<&Task> = p
        .tasks
        .iter()
        .filter(|t| matches!(t.state, TaskState::Review { binding: None, .. }))
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
