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
}

/// Side effects for the driver to execute. `InvokeModel`/`PublishContext` are part
/// of the frozen protocol but are emitted by the driver/office in W7/W9, not by the
/// W4 control loop.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Effect {
    Spawn {
        task: TaskId,
        prompt: String,
        agent: &'static str,
        model: Option<String>,
    },
    Kill {
        ext_agent_id: u64,
    },
    FetchResult {
        ext_agent_id: u64,
    },
    InvokeModel {
        req_id: u64,
        purpose: InvokePurpose,
        role: String,
        system: String,
        prompt: String,
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
                p.tasks[idx].comments.push(Comment {
                    id,
                    author,
                    text,
                    created_ms: now_ms,
                    receipt: Receipt::Pending,
                });
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
    let (system, prompt) = office::build_breakdown_prompt(p, None);
    emit_invoke(ctx, InvokePurpose::Breakdown, &p.config.office_role, system, prompt);
}

/// The hard authorization gate (6.3.3). The driver has already validated + created the
/// path; `delivery_valid` is its verdict, and `office::authorize` re-checks the shape
/// before transitioning `Ready -> Running`.
fn authorize(p: &mut Project, delivery_path: PathBuf, allow_outside: bool, now_ms: u64, ctx: &mut Ctx) {
    match office::authorize(p, delivery_path, allow_outside) {
        Ok(()) => ctx.dirty = true,
        Err(e) => {
            let notice = format!("authorization refused: {:?}; project stays in Ready", e);
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
                text: reply,
            });
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
        InvokePurpose::Breakdown => handle_breakdown_result(p, outcome, false, now_ms, ctx),
        InvokePurpose::BreakdownReask => handle_breakdown_result(p, outcome, true, now_ms, ctx),
    }
}

/// Validate a breakdown result and land it, or (first failure) re-ask once, or (second
/// failure) surface to the user (6.3.2).
fn handle_breakdown_result(p: &mut Project, outcome: Result<String, String>, is_reask: bool, now_ms: u64, ctx: &mut Ctx) {
    let text = match outcome {
        Ok(t) => t,
        Err(e) => {
            queue_notice(p, now_ms, format!("office breakdown call failed: {e}"), ctx);
            ctx.dirty = true;
            return;
        }
    };
    match office::parse_breakdown(&text) {
        Ok(breakdown) => {
            office::apply_breakdown(p, breakdown);
            ctx.dirty = true;
        }
        Err(e) => {
            if is_reask {
                queue_notice(
                    p,
                    now_ms,
                    format!("office breakdown rejected twice ({e:?}); edit the board manually"),
                    ctx,
                );
                ctx.dirty = true;
            } else {
                let (system, prompt) = office::build_breakdown_prompt(p, Some(&format!("{e:?}")));
                emit_invoke(ctx, InvokePurpose::BreakdownReask, &p.config.office_role, system, prompt);
            }
        }
    }
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
    });
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
    let idx = match find_by_agent(p, agent_id) {
        Some(i) => i,
        None => return,
    };
    match binding_kind(&p.tasks[idx].state) {
        Some(AgentKind::Worker) => on_worker_result(p, idx, text, now_ms, ctx),
        Some(AgentKind::Reviewer) => on_reviewer_result(p, idx, text, now_ms, ctx),
        None => {}
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
            maybe_complete_project(p, now_ms);
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

fn spawn_worker(p: &mut Project, tid: &TaskId, bound: &str, delivery: &Path, now_ms: u64, ctx: &mut Ctx) {
    let idx = match find_task(p, tid) {
        Some(i) => i,
        None => return,
    };
    let workspace = match &p.workspace {
        Some(w) => w.clone(),
        None => return,
    };
    let desk = workspace
        .join("koma-workflow")
        .join("desks")
        .join(&tid.0);
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

    ctx.fx.push(Effect::EnsureDesk {
        task: tid.clone(),
        dir: desk.clone(),
    });
    ctx.fx.push(Effect::Spawn {
        task: tid.clone(),
        prompt,
        agent: "office-worker",
        model: p.config.worker_model.clone(),
    });

    p.tasks[idx].desk = Some(desk);
    p.tasks[idx].state = TaskState::OnProgress {
        binding: AgentBinding {
            ext_agent_id: PROVISIONAL,
            session: bound.to_string(),
            spawned_at_ms: now_ms,
            kind: AgentKind::Worker,
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
        agent: "office-reviewer",
        model: p.config.reviewer_model.clone(),
    });

    p.tasks[idx].state = TaskState::Review {
        binding: Some(AgentBinding {
            ext_agent_id: PROVISIONAL,
            session: bound.to_string(),
            spawned_at_ms: now_ms,
            kind: AgentKind::Reviewer,
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

/// If every task is Done, close the project.
fn maybe_complete_project(p: &mut Project, now_ms: u64) {
    if matches!(p.phase, ProjectPhase::Running)
        && p.tasks.iter().all(|t| matches!(t.state, TaskState::Done { .. }))
        && !p.tasks.is_empty()
    {
        if let Ok(ph) = step_project(&p.phase, ProjectTransition::Complete { at_ms: now_ms }) {
            p.phase = ph;
        }
    }
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
