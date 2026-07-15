//! The driver: wires the pure kernel (`office-core`) + the durable store
//! (`office-store`) + a live host into the running tick loop (BUILD_WAVES.md W7,
//! ARCHITECTURE.md 5.1 / 9.1 / 10.2).
//!
//! The kernel is a pure per-`Project` function; the driver is the impure shell that
//! owns every project this instance leases, feeds inputs into `kernel::step`, and
//! EXECUTES the returned effects against the host trait. It also performs start-up
//! reconciliation (9.1), threads the session-global sub-agent capacity into the
//! kernel (5.2.3), and pushes full-snapshot updates to the panel (10.2/10.3).
//!
//! ## Why generic over [`Host`]
//! Production runs against `KomaHost` (a live `Koma`); `driver_test.rs` runs against
//! `FakeHost` (scripted replies + a recorded call log). Everything below takes an
//! explicit `now_ms` so the tests are fully deterministic — the real loop reads the
//! wall clock once per iteration and threads it in.
//!
//! ## Off-loop work (deadlock rule, 5.1)
//! `Koma::call` blocks up to 120s and `models.invoke` up to ~360s (wire cap
//! `EXT_MODELS_CALL_TIMEOUT`; broker inner ~330s); neither may stall the tick loop. The lease heartbeat runs on its OWN thread ([`heartbeat_owned`]) so
//! a slow host call can never age the lease past the 60s steal window (4.4, 5.6), and
//! `InvokeModel` effects are handed to a worker pool in W9 (the kernel emits none in
//! W7, so the effect arm here is a documented no-op).

use std::collections::{HashMap, VecDeque};
use std::path::PathBuf;
use std::sync::mpsc::Sender;
use std::sync::{Mutex, OnceLock};

use koma_extension::Koma;
use office_core::digest::{context_blob, panel_snapshot_with_activity, OfficeActivity};
use office_core::office::{self, InvokePurpose};
use office_core::{
    kernel, CommentAuthor, CommentId, Effect, Project, ProjectPhase, SnapshotMode, TaskId,
    TaskState,
};
use office_store::{lease, Lease, Store};
use serde_json::{json, Value};

use crate::handlers::{Command as DCmd, HostEvent as DEvt, Input as DInput};
use crate::host::{is_grant_denied, Host};
use crate::inbox::{self, InboxOutcome};

/// The office self-caps its concurrent sub-agents at 4 so one of the host's 5
/// per-session `MAX_SUBAGENTS` slots is always reserved for the user (5.2.3). This is
/// SESSION-GLOBAL: the union of running workers + reviewers across every project bound
/// to this session must stay under it.
const SESSION_AGENT_CAP: u32 = 4;

/// Reconcile (agents.status poll + orphan sweep + runtime ceiling) cadence (5.2.1).
const RECONCILE_MS: u64 = 30_000;

/// Panel push throttle (10.2): at most one push per this window; dirty flags coalesce.
const PUSH_THROTTLE_MS: u64 = 250;

/// Serialized-envelope size beyond which the driver drops to summary mode and sets
/// `truncated: true` (10.2). Kept under the SDK's 1MiB `panel_push` hard drop.
const PANEL_PUSH_GUARD: usize = 900_000;

/// Max concurrent off-loop `models.invoke` calls (5.1 / 6.2): each runs on its own worker
/// thread holding a `try_clone`'d `Koma`. Bounded so a burst of PRD/breakdown/fold invokes
/// cannot exhaust threads or starve the host; the rest queue and start as slots free.
const INVOKE_POOL_CAP: usize = 2;

// ---------------------------------------------------------------------------
// Inline-reply snapshot cache (panel sync reads, 10.2 section 1.1)
// ---------------------------------------------------------------------------

/// Latest board envelope + PRD bodies, kept fresh by the driver so `on_invoke`
/// (a different thread, bound by the deadlock rule) can answer `hello`/`state`/
/// `prd_get` INLINE without ever touching a `Koma` handle or the tick loop.
#[derive(Default)]
pub struct SnapshotCache {
    pub board: Option<Value>,
    pub prds: HashMap<String, String>,
}

/// Process-global cache. Populated once by `driver_entry` before the serve loop starts
/// (handlers tolerate it being empty during the boot window).
pub static CACHE: OnceLock<Mutex<SnapshotCache>> = OnceLock::new();

/// The board envelope for an inline `hello`/`state` reply, or an empty snapshot when the
/// daemon has not booted yet.
pub fn cache_snapshot() -> Value {
    if let Some(m) = CACHE.get() {
        if let Ok(g) = m.lock() {
            if let Some(b) = &g.board {
                return b.clone();
            }
        }
    }
    empty_envelope()
}

/// The PRD markdown for an inline `prd_get` reply (`null` when unknown).
pub fn cache_prd(slug: &str) -> Value {
    if let Some(m) = CACHE.get() {
        if let Ok(g) = m.lock() {
            if let Some(p) = g.prds.get(slug) {
                return Value::String(p.clone());
            }
        }
    }
    Value::Null
}

fn empty_envelope() -> Value {
    json!({ "kind": "snapshot", "seq": 0, "truncated": false, "projects": [] })
}

// ---------------------------------------------------------------------------
// Clock + instance helpers
// ---------------------------------------------------------------------------

/// Wall-clock milliseconds since the unix epoch.
pub fn now_ms() -> u64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// A per-process instance id for the lease (4.4). Not a real uuid4 — any value unique to
/// this process suffices, and pid+nanos+addr is unique enough without a uuid dependency.
pub fn mint_instance() -> String {
    let pid = std::process::id();
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let salt = &nanos as *const _ as usize;
    format!("inst-{:x}-{:x}-{:x}", pid, nanos, salt)
}

// ---------------------------------------------------------------------------
// Driver
// ---------------------------------------------------------------------------

/// One project this instance knows about, plus whether we hold its dispatch lease.
struct Owned {
    project: Project,
    /// `Some` when we hold the lease and may dispatch; `None` = read-only (comments only).
    lease: Option<Lease>,
}

/// A single off-loop `models.invoke` job (5.1 / W9). Carried by the [`Invoker`] to a
/// worker thread; the result returns as `handlers::Command::InvokeDone`.
#[derive(Clone, Debug)]
pub struct InvokeJob {
    /// Driver-minted id, matched against the pending map on completion (and on retry).
    pub req_id: u64,
    /// When this invoke was submitted (ms), used to derive live "office activity" elapsed time.
    pub submitted_at_ms: u64,
    /// The project slug this invoke belongs to (resolved to an index on completion, so a
    /// reorder of the projects vec cannot misroute a late result).
    pub proj_slug: String,
    /// What the kernel should do with the result (echoed into `kernel::Command::InvokeResult`).
    pub purpose: InvokePurpose,
    pub role: String,
    pub system: String,
    pub prompt: String,
    /// Whether the one timeout retry has already been spent (5.1 / 6.2).
    pub retried: bool,
    /// The `models.invoke` output format (feature 5), forwarded as the `format` param when
    /// `Some` — the host maps `"json"` to a chat-completions `response_format: json_object`;
    /// other dialects ignore it. `None` for the prose invokes (persona/TRD/CRD/fold).
    pub format: Option<String>,
}

/// Runs an [`InvokeJob`] OFF the driver tick loop (5.1). Production spawns a worker thread
/// on a `try_clone`'d `Koma`; tests install a recording fake so the pool's routing, retry,
/// and cap logic can be driven deterministically without threads or a live host. The
/// result MUST arrive back as a `handlers::Command::InvokeDone { req_id, result }` on the
/// driver's input channel.
pub trait Invoker: Send {
    fn run(&mut self, job: InvokeJob);
}

/// Default no-op invoker: drops every job. Used until the real pool is installed
/// (`set_invoker`), and by the W7 driver tests that never emit an `InvokeModel` effect.
struct NoopInvoker;
impl Invoker for NoopInvoker {
    fn run(&mut self, _job: InvokeJob) {}
}

/// Production invoker: each `run` spawns a thread holding its own `try_clone`'d `Koma`,
/// performs the `models.invoke` (up to the ~330s broker-inner budget), and posts the outcome back on `tx` as an
/// `InvokeDone` command. The driver's `INVOKE_POOL_CAP` bounds how many run at once.
pub struct ThreadInvoker {
    koma: Koma,
    tx: Sender<DInput>,
}

impl ThreadInvoker {
    pub fn new(koma: Koma, tx: Sender<DInput>) -> Self {
        ThreadInvoker { koma, tx }
    }
}

impl Invoker for ThreadInvoker {
    fn run(&mut self, job: InvokeJob) {
        let mut koma = self.koma.try_clone();
        let tx = self.tx.clone();
        std::thread::spawn(move || {
            let mut params = json!({ "prompt": job.prompt });
            if !job.role.is_empty() {
                params["role"] = json!(job.role);
            }
            if !job.system.is_empty() {
                params["system"] = json!(job.system);
            }
            // Feature 5: structured-output invokes ask for JSON; chat-completions dialects honor
            // it (response_format json_object), others silently ignore it.
            if let Some(fmt) = &job.format {
                params["format"] = json!(fmt);
            }
            let reply = koma.call("models.invoke", params);
            let result = match reply.get("output").and_then(Value::as_str) {
                Some(o) => Ok(o.to_string()),
                None => Err(reply
                    .get("error")
                    .and_then(Value::as_str)
                    .unwrap_or("models.invoke failed")
                    .to_string()),
            };
            let _ = tx.send(DInput::Command(crate::handlers::Command::InvokeDone {
                req_id: job.req_id,
                result,
            }));
        });
    }
}

/// Outcome of executing a single `Spawn` effect against the host.
enum SpawnOutcome {
    /// Local, tracked spawn — the real agent id was recorded onto the binding.
    Local,
    /// The bound session moved to another daemon (`{status:"sent"}`): a cross-process
    /// fire-and-forget that we must NOT treat as ours (5.6). Dispatch short-circuits.
    CrossProcess,
    /// The spawn errored before minting an agent (fed back as `SpawnFailed`).
    Failed,
}

pub struct Driver<H: Host> {
    store: Store,
    /// Public so `driver_test.rs` can script replies and assert the recorded call log.
    pub host: H,
    instance: String,
    pid: u32,
    projects: Vec<Owned>,
    /// This daemon's own session id (`sessions.list`[0] under daemon-per-session, 2.2).
    session: Option<String>,
    workspace: Option<PathBuf>,
    last_reconcile_ms: u64,
    last_push_ms: u64,
    push_seq: u64,
    /// A push was suppressed by the throttle and still owes a flush.
    push_pending: bool,
    /// State changed since the last context republish.
    ctx_dirty: bool,
    /// Off-loop invoke execution (5.1). `NoopInvoker` until `set_invoker` installs the
    /// real thread pool (production) or a fake (tests).
    invoker: Box<dyn Invoker>,
    /// Invoke jobs issued but not yet terminally routed to the kernel (keyed by req_id).
    /// Retained across a retry so the same job can be re-run without re-minting an id.
    invoke_pending: HashMap<u64, InvokeJob>,
    /// req_ids waiting for a free pool slot (the pool was at `INVOKE_POOL_CAP`).
    invoke_queue: VecDeque<u64>,
    /// How many invoke jobs are currently running on the pool.
    invoke_in_flight: usize,
    /// Monotonic invoke request id source.
    next_req_id: u64,
}

impl<H: Host> Driver<H> {
    /// Load every project from the store (self-healing adopt/quarantine, W5) into a fresh
    /// driver. Does NOT touch the host — call [`Driver::bootstrap`] for session binding,
    /// lease acquisition, and reconciliation.
    pub fn load(store: Store, host: H, instance: String, pid: u32) -> std::io::Result<Self> {
        let loaded = store.load_all()?;
        let projects = loaded
            .projects
            .into_iter()
            .map(|project| Owned {
                project,
                lease: None,
            })
            .collect();
        Ok(Driver {
            store,
            host,
            instance,
            pid,
            projects,
            session: None,
            workspace: None,
            last_reconcile_ms: 0,
            last_push_ms: 0,
            push_seq: 0,
            push_pending: false,
            ctx_dirty: false,
            invoker: Box::new(NoopInvoker),
            invoke_pending: HashMap::new(),
            invoke_queue: VecDeque::new(),
            invoke_in_flight: 0,
            next_req_id: 0,
        })
    }

    /// Install the off-loop invoke pool (production `ThreadInvoker`, or a test fake). The
    /// driver starts with a `NoopInvoker`, so this must be called before any persona /
    /// PRD / breakdown flow can run.
    pub fn set_invoker(&mut self, invoker: Box<dyn Invoker>) {
        self.invoker = invoker;
    }

    // -- boot ---------------------------------------------------------------

    /// Start-up sequence (ARCHITECTURE.md 9.1): bind this daemon's session via
    /// `sessions.list`, acquire leases, reconcile in-flight agents, republish the context
    /// blob, and push the first snapshot. Idempotent enough to re-run on a re-lease.
    pub fn bootstrap(&mut self, now_ms: u64) {
        // 9.1.2 — bind our session (daemon-per-session: exactly one row).
        let sessions = self.host.call("sessions.list", json!({}));
        self.session = first_session_id(&sessions);
        self.workspace = first_session_workdir(&sessions).map(PathBuf::from);

        // 9.1.2 — acquire or observe each lease. By index to avoid a self borrow clash.
        // The `session` passed to `lease::acquire` is THIS DAEMON'S OWN session (4.4:
        // "session == our local session"), never the project's `bound_session` — otherwise
        // the same-session-rebind clause degenerates into a tautology (lease.session was
        // itself written from a project's bound_session, so it always equals the project's
        // current bound_session and any daemon can "rebind" any live foreign lease).
        for i in 0..self.projects.len() {
            let slug = self.projects[i].project.id.0.clone();
            let path = self.store.lease_path(&slug);
            let lease = lease::acquire(&path, &self.instance, self.session.as_deref(), self.pid, now_ms)
                .ok()
                .flatten();
            self.projects[i].lease = lease;
        }

        // Seed the PRD cache so inline `prd_get` replies work immediately.
        self.reload_prd_cache();

        // 9.1.3 + 9.1.4 — reconcile stale bindings + sweep orphans.
        self.reconcile(now_ms);
        self.last_reconcile_ms = now_ms;

        // 9.1.5 — republish context + first snapshot.
        self.republish_context();
        self.push_board(now_ms, true);
    }

    // -- the loop tick ------------------------------------------------------

    /// One `recv_timeout` tick: rate-limited reconcile, the inbox poll, a dispatch scan
    /// for every owned Running project, then one outbox drain.
    pub fn on_tick(&mut self, now_ms: u64) {
        if now_ms.saturating_sub(self.last_reconcile_ms) >= RECONCILE_MS {
            self.reconcile(now_ms);
            self.last_reconcile_ms = now_ms;
        }
        self.poll_inbox(now_ms);
        self.poll_global_inbox(now_ms);
        for i in self.owned_running_indices() {
            self.step(i, kernel::Input::Host(kernel::HostEvent::Tick), now_ms);
        }
        self.drain_outbox(now_ms);
        if self.push_pending {
            self.push_board(now_ms, true);
        }
        if self.ctx_dirty {
            self.republish_context();
            self.ctx_dirty = false;
        }
    }

    /// Route one daemon-level [`Input`](crate::handlers::Input) (a parsed tool/panel
    /// command or a host event) into the kernel.
    pub fn handle(&mut self, input: DInput, now_ms: u64) {
        match input {
            DInput::Command(c) => self.handle_command(c, now_ms),
            DInput::Event(e) => self.handle_event(e, now_ms),
        }
    }

    // -- command routing ----------------------------------------------------

    fn handle_command(&mut self, c: DCmd, now_ms: u64) {
        match c {
            DCmd::Interrupt { project, hard } => {
                if let Some(i) = self.owned_project_by_id(&project) {
                    self.step(i, kernel::Input::Command(kernel::Command::Interrupt { hard }), now_ms);
                }
            }
            DCmd::Resume { project } => {
                if let Some(i) = self.owned_project_by_id(&project) {
                    self.step(i, kernel::Input::Command(kernel::Command::Resume), now_ms);
                }
            }
            DCmd::Unpark { task } => {
                let tid = TaskId(task);
                if let Some(i) = self.owned_project_by_task(&tid) {
                    self.step(i, kernel::Input::Command(kernel::Command::Unpark { task: tid }), now_ms);
                }
            }
            DCmd::Comment { task, text } => self.add_comment(&TaskId(task), text, now_ms),
            // ---- front office (W9, ARCHITECTURE.md 6.2 / 6.3) ----
            DCmd::Brief { project, message } => {
                // A brief is the documented way to START a project ("use
                // workflow_brief to get started"), so an id that doesn't resolve
                // — or no projects at all — must MINT a Drafting project rather
                // than silently dropping the message (live-test bug 2026-07-15:
                // brief acked "office is thinking" and then went nowhere).
                let idx = self.resolve_office_project(project.as_deref()).or_else(|| {
                    let name = project
                        .clone()
                        .unwrap_or_else(|| derive_project_name(&message));
                    self.create_project(name, now_ms)
                });
                if let Some(i) = idx {
                    self.step(
                        i,
                        kernel::Input::Command(kernel::Command::OfficeMessage { text: message }),
                        now_ms,
                    );
                }
            }
            DCmd::OfficeChat { project, message } => {
                if let Some(i) = self.owned_project_by_id(&project) {
                    self.step(
                        i,
                        kernel::Input::Command(kernel::Command::OfficeMessage { text: message }),
                        now_ms,
                    );
                }
            }
            DCmd::Authorize {
                project,
                delivery_path,
            } => self.authorize_project(&project, &delivery_path, now_ms),
            DCmd::Breakdown { project } => {
                if let Some(i) = self.owned_project_by_id(&project) {
                    self.step(i, kernel::Input::Command(kernel::Command::RequestBreakdown), now_ms);
                }
            }
            // An off-loop invoke completed: apply the driver's one retry, else route the
            // outcome into the kernel (5.1 / 6.2).
            DCmd::InvokeDone { req_id, result } => self.on_invoke_done(req_id, result, now_ms),
            // A fresh project from the panel's "New Project" affordance (10.2, 6.1):
            // construct an empty Drafting project so the PRD -> breakdown -> authorize ->
            // Running pipeline has something to act on.
            DCmd::ProjectCreate { name } => {
                let _ = self.create_project(name, now_ms);
            }
            // A direct project-config edit (10.2 `config_set`). Only the lease holder
            // may apply it, matching Interrupt/Resume/Unpark above; a non-holder's
            // config_set is silently dropped the same way theirs would be.
            DCmd::ConfigSet {
                project,
                max_workers,
                bounce_budget,
                worker_model,
                reviewer_model,
                keep_desks,
                crd_pass_grade,
                assumption_check,
            } => {
                if let Some(i) = self.owned_project_by_id(&project) {
                    self.step(
                        i,
                        kernel::Input::Command(kernel::Command::ConfigSet {
                            max_workers,
                            bounce_budget,
                            worker_model,
                            reviewer_model,
                            keep_desks,
                            crd_pass_grade,
                            assumption_check,
                        }),
                        now_ms,
                    );
                }
            }
            // Manual project delete (Settings panel "danger zone"): lease-holder only,
            // mirroring Interrupt/Resume/Unpark/ConfigSet above.
            DCmd::ProjectArchive { project } => self.archive_project(&project, now_ms),
            // Remaining panel/tool surface not owned by W9; no-ops until their waves wire
            // them (status digests, board edits, project lifecycle).
            DCmd::Status { .. }
            | DCmd::Projects
            | DCmd::CardMove { .. }
            | DCmd::EditTask { .. }
            | DCmd::EditDeps { .. }
            | DCmd::TaskDetail { .. } => {}
            // hello/state/prd_get are answered INLINE by the handler off the snapshot
            // cache (10.2 section 1.1) and never reach the driver; matched for totality.
            DCmd::PanelHello { .. } | DCmd::PanelState { .. } | DCmd::PrdGet { .. } => {}
        }
    }

    /// A board comment. The lease holder folds it through the kernel; a read-only instance
    /// (non-holder) may still add it via the flock'd store path (4.4).
    fn add_comment(&mut self, task: &TaskId, text: String, now_ms: u64) {
        if let Some(i) = self.owned_project_by_task(task) {
            self.step(
                i,
                kernel::Input::Command(kernel::Command::AddComment {
                    task: task.clone(),
                    author: CommentAuthor::User,
                    text,
                }),
                now_ms,
            );
            return;
        }
        // Not owned: comment through the advisory lock so a cross-instance add can't tear.
        if let Some(i) = self.project_by_task(task) {
            let slug = self.projects[i].project.id.0.clone();
            let tid = task.clone();
            let _ = self.store.with_state_lock(&slug, |p| {
                if let Some(t) = p.tasks.iter_mut().find(|t| &t.id == &tid) {
                    let id = office_core::CommentId(
                        t.comments.iter().map(|c| c.id.0).max().unwrap_or(0) + 1,
                    );
                    t.comments.push(office_core::Comment {
                        id,
                        author: CommentAuthor::User,
                        text,
                        created_ms: now_ms,
                        receipt: office_core::Receipt::Pending,
                    });
                }
            });
            // Reflect the on-disk change locally and repaint.
            if let Ok(p) = self.store.load_project(&slug) {
                self.projects[i].project = p;
            }
            self.push_board(now_ms, false);
        }
    }

    // -- inbox (6.4) ----------------------------------------------------------

    /// Poll `<workspace>/koma-workflow/inbox/` for files the main chat dropped (the
    /// daemon-mode reach path, since contributed tools are invisible to the model in
    /// `--daemon` sessions — Limitation 1). Accepted files are routed through the same
    /// `handle_command` path a `tool.call` invoke would take; every consumed file
    /// (accepted or rejected) gets a best-effort `chat.prompt` acknowledgement, since
    /// an inbox drop has no synchronous caller to reply to directly (unlike
    /// `tool.call`, whose ack is the invoke's own return value). No-ops before the
    /// workspace is known (pre-bootstrap).
    fn poll_inbox(&mut self, now_ms: u64) {
        let Some(ws) = self.workspace.clone() else {
            return;
        };
        let dir = ws.join("koma-workflow").join("inbox");
        for outcome in inbox::poll(&dir, inbox::MAX_FILES_PER_TICK) {
            match outcome {
                InboxOutcome::Accepted { command, ack, .. } => {
                    self.handle_command(command, now_ms);
                    self.host.call("chat.prompt", json!({ "text": ack }));
                }
                InboxOutcome::Rejected { file, reason } => {
                    self.host.call(
                        "chat.prompt",
                        json!({ "text": format!("workflow: could not process {file}: {reason}") }),
                    );
                }
            }
        }
    }

    /// Poll the GLOBAL inbox (`<state-root>/inbox`) — the dir the `workflow-mcp` server
    /// writes to when no workspace is in scope — with ownership-aware, race-safe claiming.
    /// Runs AFTER `poll_inbox` in the tick, so the per-workspace inbox behavior above is
    /// untouched. Because the global inbox is SHARED across every koma instance, this
    /// instance claims only files addressed to a project it OWNS — plus new/unknown-project
    /// briefs (which it mints locally) and undeterminable-malformed files (which it rejects)
    /// — and LEAVES everything else in place for the owning instance. Claimed files are
    /// routed through the same `handle_command` + `chat.prompt` acknowledgement path as the
    /// workspace inbox. Independent of `self.workspace`, so it works pre-bootstrap too.
    fn poll_global_inbox(&mut self, now_ms: u64) {
        let dir = self.store.root_dir().join("inbox");
        // The registry lets `global_claim` tell an UNKNOWN project id (a brief may mint it)
        // from a KNOWN one owned by a DIFFERENT instance (leave it). Snapshot it once.
        let known: std::collections::HashSet<String> = self
            .store
            .registry()
            .map(|rows| rows.into_iter().map(|r| r.project_id).collect())
            .unwrap_or_default();

        // The ownership predicate borrows `self` immutably; it (and its borrow) is dropped
        // when `poll_global` returns, before the mutable `handle_command` loop below.
        let outcomes = inbox::poll_global(&dir, inbox::MAX_FILES_PER_TICK, |target| {
            self.global_claim(target, &known)
        });

        for outcome in outcomes {
            match outcome {
                InboxOutcome::Accepted { command, ack, .. } => {
                    self.handle_command(command, now_ms);
                    self.host.call("chat.prompt", json!({ "text": ack }));
                }
                InboxOutcome::Rejected { file, reason } => {
                    self.host.call(
                        "chat.prompt",
                        json!({ "text": format!("workflow: could not process {file}: {reason}") }),
                    );
                }
            }
        }
    }

    /// The ownership verdict for a global-inbox file's peeked [`inbox::Target`]. Owner-only
    /// for project/task-addressed ops (`authorize`/`interrupt`/`resume`/`breakdown`/`comment`); a brief
    /// with an absent or registry-UNKNOWN project is claimable by anyone (it mints locally);
    /// an undeterminable target is claimable (to reject). `known` is the set of project ids
    /// in the registry, used to distinguish an unknown project (mintable) from one owned by
    /// a DIFFERENT instance (leave it). Read-only over `self` so it can be the `poll_global`
    /// predicate closure.
    fn global_claim(
        &self,
        target: &inbox::Target,
        known: &std::collections::HashSet<String>,
    ) -> inbox::Claim {
        use inbox::{Claim, Target};
        match target {
            Target::Brief { project } => match project {
                // No id: a "get started" brief -> mint locally, claimable by anyone.
                None => Claim::Claim,
                Some(id) => {
                    if self.owned_project_by_id(id).is_some() {
                        Claim::Claim // we own it: continue the conversation
                    } else if known.contains(id) {
                        Claim::Leave // known but owned elsewhere: leave for that instance
                    } else {
                        Claim::Claim // unknown project id: mint locally
                    }
                }
            },
            Target::Project { project } => match project {
                // Project-less op is only ever `status` (a global no-op query); let the
                // rename race pick a single claimant. A project-less authorize/interrupt/
                // resume also lands here, but `parse` rejects it (undeterminable-target
                // reject), which any instance may do.
                None => Claim::Claim,
                Some(id) => {
                    if self.owned_project_by_id(id).is_some() {
                        Claim::Claim
                    } else {
                        Claim::Leave
                    }
                }
            },
            // A comment is owner-only, resolved via the task's project prefix.
            Target::Task { task } => {
                if self.owns_task_project(task) {
                    Claim::Claim
                } else {
                    Claim::Leave
                }
            }
            // Undeterminable target (unreadable / not JSON / no op): any instance may reject.
            Target::Unknown => Claim::Claim,
        }
    }

    /// Whether this instance owns the project a task id belongs to. A task id is the full
    /// hierarchical `<project>/<epic>/<story>/<task>` (kernel.rs `desk_dir` docs), so the
    /// project is the segment before the first `/`.
    fn owns_task_project(&self, task: &str) -> bool {
        let project = task.split('/').next().unwrap_or("");
        !project.is_empty() && self.owned_project_by_id(project).is_some()
    }

    // -- event routing ------------------------------------------------------

    fn handle_event(&mut self, e: DEvt, now_ms: u64) {
        match e {
            // The private notify from `agents.spawn { notify: true }` — the ONLY event
            // carrying our ext-facing agent id (2.2). Correlate to the owning project.
            DEvt::AgentsDone { agent_id, status, error } => {
                if let Some(i) = self.owned_project_by_agent(agent_id) {
                    self.step(
                        i,
                        kernel::Input::Host(kernel::HostEvent::AgentsDone { agent_id, status, error }),
                        now_ms,
                    );
                }
            }
            // A user turn resets the host chat-prompt budget: un-pause the outbox (6.5).
            DEvt::AgentTurnEnd { .. } => {
                for o in &mut self.projects {
                    for n in &mut o.project.outbox {
                        n.paused = false;
                    }
                }
                self.drain_outbox(now_ms);
            }
            // Broadcast corroboration channels; primary correlation is `agents.done`
            // (2.2), so these are not used to drive state in v1.
            DEvt::SubagentDone { .. } | DEvt::SessionForegroundChange { .. } => {}
        }
    }

    // -- kernel step + effect execution ------------------------------------

    /// Run one `kernel::step` on project `idx` with the current session-global capacity,
    /// then execute the returned effects.
    fn step(&mut self, idx: usize, input: kernel::Input, now_ms: u64) {
        let capacity = self.remaining_capacity();
        let effects = kernel::step(&mut self.projects[idx].project, input, now_ms, capacity);
        self.execute(idx, effects, now_ms);
    }

    /// Execute an effect vector in order. A `Spawn` that reports the bound session moved
    /// off-daemon short-circuits the rest of the batch, releases the lease, and rolls the
    /// project back to its on-disk state (5.6): no untracked duplicate workers.
    fn execute(&mut self, idx: usize, effects: Vec<Effect>, now_ms: u64) {
        for fx in effects {
            match fx {
                Effect::EnsureDesk { dir, .. } => self.ensure_desk(&dir),
                Effect::Spawn {
                    task,
                    prompt,
                    agent,
                    model,
                } => match self.exec_spawn(idx, &task, prompt, agent, model, now_ms) {
                    SpawnOutcome::CrossProcess => {
                        self.abort_dispatch(idx, now_ms);
                        return;
                    }
                    SpawnOutcome::Local | SpawnOutcome::Failed => {}
                },
                Effect::SpawnResearch { prompt } => self.exec_spawn_research(idx, prompt, now_ms),
                Effect::SpawnAudit { prompt } => self.exec_spawn_audit(idx, prompt, now_ms),
                Effect::Kill { ext_agent_id } => {
                    self.host.call("agents.kill", json!({ "agentId": ext_agent_id }));
                }
                Effect::FetchResult { ext_agent_id } => self.fetch_result(idx, ext_agent_id, now_ms),
                Effect::InjectComment {
                    ext_agent_id,
                    comment_id,
                    text,
                } => self.exec_inject_comment(idx, ext_agent_id, comment_id, text, now_ms),
                Effect::QueueChatPrompt { .. } => {
                    // The notice is already durable in `project.outbox`; the tick's
                    // `drain_outbox` sends it under the 6.5 budget discipline.
                    self.drain_outbox(now_ms);
                }
                Effect::PanelPush { .. } => self.push_board(now_ms, false),
                Effect::Persist => {
                    let _ = self.store.save_project(&self.projects[idx].project);
                    self.sync_prd_cache(idx);
                    self.ctx_dirty = true;
                }
                Effect::PublishContext { text } => {
                    self.host.call("context.set", json!({ "text": text }));
                }
                // Hand the invoke to the off-loop pool (5.1): NEVER run inline on the tick
                // loop. `req_id` on the effect is a kernel placeholder; the driver mints
                // the real id here.
                Effect::InvokeModel {
                    purpose,
                    role,
                    system,
                    prompt,
                    format,
                    ..
                } => self.submit_invoke(idx, purpose, role, system, prompt, format, now_ms),
            }
        }
    }

    /// Execute a single `Spawn`: `sessions.spawn_into` on the bound session, then feed the
    /// real agent id back as `Spawned` (or `SpawnFailed`). `{status:"sent"}` = cross-process.
    fn exec_spawn(
        &mut self,
        idx: usize,
        task: &TaskId,
        prompt: String,
        // Owned now (was `&'static str`): a worker spawn carries a per-task persona id
        // (`office-worker-<name>`), a reviewer spawn the fixed `office-reviewer`.
        agent: String,
        model: Option<String>,
        now_ms: u64,
    ) -> SpawnOutcome {
        let bound = self.projects[idx].project.bound_session.clone().unwrap_or_default();
        let mut params = json!({
            "session": bound,
            "task": prompt,
            "agent": agent,
            "notify": true,
        });
        if let Some(m) = model {
            if !m.is_empty() {
                params["model"] = json!(m);
            }
        }
        let reply = self.host.call("sessions.spawn_into", params);

        if reply.get("status").and_then(Value::as_str) == Some("sent") {
            return SpawnOutcome::CrossProcess;
        }
        if let Some(agent_id) = parse_agent_id(&reply) {
            self.step(
                idx,
                kernel::Input::Host(kernel::HostEvent::Spawned {
                    task: task.clone(),
                    agent_id,
                    spawned_at_ms: now_ms,
                }),
                now_ms,
            );
            return SpawnOutcome::Local;
        }
        let reason = error_str(&reply).unwrap_or("spawn failed").to_string();
        self.step(
            idx,
            kernel::Input::Host(kernel::HostEvent::SpawnFailed {
                task: task.clone(),
                reason,
            }),
            now_ms,
        );
        SpawnOutcome::Failed
    }

    /// Execute a `SpawnResearch` (6.2b): spawn the `office-researcher` into the project's bound
    /// session via the SAME `sessions.spawn_into` path as workers, then feed the real agent id
    /// back as `ResearchSpawned`. Unlike a worker spawn there is no duplicate-worker hazard
    /// (5.6) — the project is Drafting, not dispatching a ready set — so a cross-process
    /// `{status:"sent"}` reply or a spawn error just degrades gracefully via `ResearchFailed`
    /// (Drafting drops to a PRD-only TRD) and NEVER releases the lease.
    fn exec_spawn_research(&mut self, idx: usize, prompt: String, now_ms: u64) {
        let bound = self.projects[idx].project.bound_session.clone().unwrap_or_default();
        let params = json!({
            "session": bound,
            "task": prompt,
            "agent": "office-researcher",
            "notify": true,
        });
        let reply = self.host.call("sessions.spawn_into", params);

        if reply.get("status").and_then(Value::as_str) == Some("sent") {
            self.step(
                idx,
                kernel::Input::Host(kernel::HostEvent::ResearchFailed {
                    reason: "bound session moved off-daemon".to_string(),
                }),
                now_ms,
            );
            return;
        }
        if let Some(agent_id) = parse_agent_id(&reply) {
            self.step(
                idx,
                kernel::Input::Host(kernel::HostEvent::ResearchSpawned {
                    agent_id,
                    spawned_at_ms: now_ms,
                }),
                now_ms,
            );
            return;
        }
        let reason = error_str(&reply).unwrap_or("spawn failed").to_string();
        self.step(
            idx,
            kernel::Input::Host(kernel::HostEvent::ResearchFailed { reason }),
            now_ms,
        );
    }

    /// Execute a `SpawnAudit` (6.2c): spawn the read-only `office-auditor` into the project's
    /// bound session via the SAME `sessions.spawn_into` path as the researcher, then feed the real
    /// agent id back as `AuditSpawned`. Like the researcher (and unlike a worker spawn) there is
    /// no duplicate-worker hazard — a cross-process `{status:"sent"}` reply or a spawn error just
    /// degrades to Done via `AuditFailed` and NEVER releases the lease.
    fn exec_spawn_audit(&mut self, idx: usize, prompt: String, now_ms: u64) {
        let bound = self.projects[idx].project.bound_session.clone().unwrap_or_default();
        let params = json!({
            "session": bound,
            "task": prompt,
            "agent": "office-auditor",
            "notify": true,
        });
        let reply = self.host.call("sessions.spawn_into", params);

        if reply.get("status").and_then(Value::as_str) == Some("sent") {
            self.step(
                idx,
                kernel::Input::Host(kernel::HostEvent::AuditFailed {
                    reason: "bound session moved off-daemon".to_string(),
                }),
                now_ms,
            );
            return;
        }
        if let Some(agent_id) = parse_agent_id(&reply) {
            self.step(
                idx,
                kernel::Input::Host(kernel::HostEvent::AuditSpawned {
                    agent_id,
                    spawned_at_ms: now_ms,
                }),
                now_ms,
            );
            return;
        }
        let reason = error_str(&reply).unwrap_or("spawn failed").to_string();
        self.step(
            idx,
            kernel::Input::Host(kernel::HostEvent::AuditFailed { reason }),
            now_ms,
        );
    }

    /// Roll a project back to its on-disk state and release its lease after a cross-process
    /// spawn reply. The optimistic in-memory dispatch was never persisted (the trailing
    /// `Persist` is skipped by the caller's early return), so the store is the clean truth.
    fn abort_dispatch(&mut self, idx: usize, now_ms: u64) {
        let slug = self.projects[idx].project.id.0.clone();
        if let Ok(p) = self.store.load_project(&slug) {
            self.projects[idx].project = p;
        }
        let path = self.store.lease_path(&slug);
        let _ = lease::release(&path, &self.instance);
        self.projects[idx].lease = None;
        self.push_board(now_ms, false);
    }

    /// `agents.result` on a terminal agent, feeding the report text back to the kernel.
    fn fetch_result(&mut self, idx: usize, agent_id: u64, now_ms: u64) {
        let reply = self.host.call("agents.result", json!({ "agentId": agent_id }));
        let text = reply
            .get("output")
            .and_then(Value::as_str)
            .map(str::to_string)
            .or_else(|| error_str(&reply).map(str::to_string))
            .unwrap_or_default();
        self.step(
            idx,
            kernel::Input::Host(kernel::HostEvent::Result { agent_id, text }),
            now_ms,
        );
    }

    /// Execute an `InjectComment` (feature 4): push a mid-run board comment to a live sub-agent
    /// via `agents.send`. On a success reply (`{"sent":true}` / `{"sent":true,"status":"queued"}`)
    /// feed `CommentDelivered` back so the kernel flips the comment `Pending -> Delivered`. Any
    /// error reply (agent terminal / unknown id / session closed) is swallowed: the comment stays
    /// `Pending` and the existing spawn-boundary fold delivers it on the next attempt — one shot
    /// per comment at add time, no retry loop.
    fn exec_inject_comment(
        &mut self,
        idx: usize,
        ext_agent_id: u64,
        comment_id: CommentId,
        text: String,
        now_ms: u64,
    ) {
        // Resolve the owning task from the live binding so the success event can address it (a
        // comment id is per-task, so the kernel needs the task to disambiguate). The binding was
        // live when the kernel emitted this effect; guard against it having vanished since.
        let task = match task_bound_to(&self.projects[idx].project, ext_agent_id) {
            Some(t) => t.id.clone(),
            None => return,
        };
        let reply = self.host.call(
            "agents.send",
            json!({ "agentId": ext_agent_id, "message": text }),
        );
        if reply.get("sent").and_then(Value::as_bool) == Some(true) {
            self.step(
                idx,
                kernel::Input::Host(kernel::HostEvent::CommentDelivered { task, comment_id }),
                now_ms,
            );
        }
    }

    // -- front office (6.2 / 6.3) ------------------------------------------

    /// Resolve the project a `workflow_brief` targets: an explicit id, or (when absent)
    /// the single owned project still in `Drafting` (6.4 default).
    pub(crate) fn resolve_office_project(&self, project: Option<&str>) -> Option<usize> {
        if let Some(id) = project {
            return self.owned_project_by_id(id);
        }
        self.projects
            .iter()
            .position(|o| o.lease.is_some() && matches!(o.project.phase, ProjectPhase::Drafting))
    }

    /// The authorization gate (6.3.3): validate the delivery path, `mkdir -p` it when
    /// valid, then hand the gate to the kernel (which re-checks and transitions, or queues
    /// a refusal notice). The escape hatch defaults off at the wire in v1.
    fn authorize_project(&mut self, project: &str, delivery_path: &str, now_ms: u64) {
        let idx = match self.owned_project_by_id(project) {
            Some(i) => i,
            None => return,
        };
        let path = PathBuf::from(delivery_path);
        let allow_outside = false;
        let workspace = self.projects[idx].project.workspace.clone();
        if office::validate_delivery_path(&path, workspace.as_deref(), allow_outside).is_ok() {
            let _ = std::fs::create_dir_all(&path);
        }
        self.step(
            idx,
            kernel::Input::Command(kernel::Command::Authorize {
                delivery_path: path,
                allow_outside_workspace: allow_outside,
            }),
            now_ms,
        );
    }

    // -- project lifecycle (6.1 / 10.2) ------------------------------------

    /// Create a fresh project from a panel `project_create` op: mint a unique slug from
    /// `name`, persist an empty `Drafting` project (state.json + registry row) via the
    /// store, acquire its dispatch lease using THIS daemon's own session (never a
    /// foreign `bound_session`, see `bootstrap`), register it in memory, and repaint the
    /// board. `bound_session`/`workspace` are seeded from this daemon so the later
    /// authorize -> Running pipeline can dispatch (dispatch bails without a bound session
    /// and a delivery path — kernel.rs `dispatch`). A store write failure aborts without
    /// registering an in-memory project so on-disk and in-memory never diverge.
    fn create_project(&mut self, name: String, now_ms: u64) -> Option<usize> {
        let slug = self.mint_project_slug(&name);
        let project = Project {
            id: office_core::ProjectId(slug.clone()),
            name,
            phase: ProjectPhase::Drafting,
            prd_markdown: String::new(),
            trd_markdown: String::new(),
            research_notes: String::new(),
            research: None,
            crd_markdown: String::new(),
            audit: None,
            audit_rounds: 0,
            last_audit_grade: None,
            pending_assumptions: Vec::new(),
            office_transcript: Vec::new(),
            office_summary: String::new(),
            delivery_path: None,
            bound_session: self.session.clone(),
            workspace: self.workspace.clone(),
            epics: Vec::new(),
            stories: Vec::new(),
            tasks: Vec::new(),
            config: office_core::ProjectConfig::default_config(),
            outbox: Vec::new(),
            seq: 0,
        };
        if self.store.create_project(&project).is_err() {
            log_line("project_create: store write failed; project not created");
            return None;
        }
        let path = self.store.lease_path(&slug);
        let lease = lease::acquire(&path, &self.instance, self.session.as_deref(), self.pid, now_ms)
            .ok()
            .flatten();
        self.projects.push(Owned { project, lease });
        let idx = self.projects.len() - 1;
        self.sync_prd_cache(idx);
        self.ctx_dirty = true;
        self.push_board(now_ms, true);
        Some(idx)
    }

    /// Turn a free-text project name into a unique, filesystem-safe slug: lowercase, every
    /// run of non-`[a-z0-9]` collapsed to a single `-`, leading/trailing `-` trimmed;
    /// empty result falls back to `project`. A collision with an already-loaded project
    /// appends `-2`, `-3`, ... until unique (all projects on disk are loaded into
    /// `self.projects`, so this set is authoritative).
    fn mint_project_slug(&self, name: &str) -> String {
        let mut base = String::new();
        let mut prev_dash = false;
        for c in name.chars() {
            if c.is_ascii_alphanumeric() {
                base.push(c.to_ascii_lowercase());
                prev_dash = false;
            } else if !prev_dash {
                base.push('-');
                prev_dash = true;
            }
        }
        let base = base.trim_matches('-');
        let base = if base.is_empty() { "project" } else { base };
        let taken = |cand: &str| self.projects.iter().any(|o| o.project.id.0 == cand);
        if !taken(base) {
            return base.to_string();
        }
        let mut n = 2u64;
        loop {
            let candidate = format!("{base}-{n}");
            if !taken(&candidate) {
                return candidate;
            }
            n += 1;
        }
    }

    /// Manually delete a project (Settings panel "danger zone", 10.2 `project_archive`):
    /// lease-holder only. A non-holder can't perform the delete, so instead of silently
    /// dropping it (which would leave the panel's optimistic card-removal unexplained) it
    /// posts an honest refusal chat notice and re-pushes the true board. For the holder:
    /// best-effort kills every in-flight binding — the per-task worker/reviewer bindings AND
    /// the project-level research (6.2b) / audit (6.2c) analyst bindings — deletes the
    /// project's desks working directory, releases the lease, then removes the on-disk state
    /// (state.json + registry row, `Store::archive_project`'s registry-row-before-dir-delete
    /// ordering) and the in-memory project. NEVER touches `delivery_path` — delivered code
    /// stays exactly where it was placed.
    fn archive_project(&mut self, project: &str, now_ms: u64) {
        let idx = match self.owned_project_by_id(project) {
            Some(i) => i,
            None => {
                // Not the lease holder: only the leasing session can delete this project, so
                // the delete cannot happen here. The panel already optimistically dropped the
                // card on its `{ok:true}` ack (fire-and-forget mpsc, PANEL_PROTOCOL 1.2), so
                // don't no-op silently — tell the user WHY and re-push the true board so the
                // card comes back instead of vanishing unexplained. Distinguish a project owned
                // by ANOTHER koma session from one this daemon has never loaded.
                let known = self.projects.iter().any(|o| o.project.id.0 == project);
                let text = if known {
                    format!("workflow: delete refused — project '{project}' is owned by another koma session.")
                } else {
                    format!("workflow: delete refused — no project '{project}' is loaded here.")
                };
                self.host.call("chat.prompt", json!({ "text": text }));
                self.push_board(now_ms, true);
                return;
            }
        };
        let slug = self.projects[idx].project.id.0.clone();

        // Best-effort: kill every in-flight binding so nothing keeps running against a project
        // that is about to stop existing — the per-task worker/reviewer bindings AND the
        // project-level research (6.2b) / audit (6.2c) analyst bindings, which are NOT task
        // bindings and so are missed by the task loop above.
        let mut agent_ids: Vec<u64> = self.projects[idx]
            .project
            .tasks
            .iter()
            .filter_map(|t| binding_agent_id(&t.state))
            .filter(|id| *id != 0)
            .collect();
        // `research_agent_id`/`audit_agent_id` already skip the provisional id 0.
        if let Some(id) = research_agent_id(&self.projects[idx].project) {
            agent_ids.push(id);
        }
        if let Some(id) = audit_agent_id(&self.projects[idx].project) {
            agent_ids.push(id);
        }
        for agent_id in agent_ids {
            self.host.call("agents.kill", json!({ "agentId": agent_id }));
        }

        // Best-effort: delete the desks working directory. NEVER touch delivery_path.
        if let Some(ws) = self.projects[idx].project.workspace.clone() {
            let desks_dir = ws.join("koma-workflow").join("desks").join(&slug);
            let _ = std::fs::remove_dir_all(&desks_dir);
        }

        // Release the lease before the store deletes the state dir it lives under.
        let lease_path = self.store.lease_path(&slug);
        let _ = lease::release(&lease_path, &self.instance);

        let _ = self.store.archive_project(&slug);

        self.projects.remove(idx);
        self.ctx_dirty = true;
        self.push_board(now_ms, true);
    }

    // -- off-loop invoke pool (5.1) ----------------------------------------

    /// Register a new invoke job and start it if a pool slot is free (else queue it). The
    /// job runs OFF the tick loop, so dispatch/reconcile/heartbeat/panel-reads stay
    /// responsive through the (up to several-minute) PRD/breakdown flow.
    fn submit_invoke(
        &mut self,
        idx: usize,
        purpose: InvokePurpose,
        role: String,
        system: String,
        prompt: String,
        format: Option<&'static str>,
        now_ms: u64,
    ) {
        self.next_req_id += 1;
        let req_id = self.next_req_id;
        let job = InvokeJob {
            req_id,
            submitted_at_ms: now_ms,
            proj_slug: self.projects[idx].project.id.0.clone(),
            purpose,
            role,
            system,
            prompt,
            retried: false,
            format: format.map(str::to_string),
        };
        self.invoke_pending.insert(req_id, job);
        self.start_or_queue_invoke(req_id);
        // An invoke just began: mark the board dirty so the live "office activity" label
        // (drafting/fact-checking/breaking-down) reaches the panel.
        self.push_board(now_ms, false);
    }

    fn start_or_queue_invoke(&mut self, req_id: u64) {
        if self.invoke_in_flight < INVOKE_POOL_CAP {
            if let Some(job) = self.invoke_pending.get(&req_id).cloned() {
                self.invoke_in_flight += 1;
                self.invoker.run(job);
            }
        } else {
            self.invoke_queue.push_back(req_id);
        }
    }

    /// An off-loop invoke returned. Timeout + not-yet-retried -> re-run the SAME job once
    /// (same slot, no new id). Otherwise free the slot, route the outcome into the kernel
    /// as `Command::InvokeResult`, and start the next queued job.
    fn on_invoke_done(&mut self, req_id: u64, result: Result<String, String>, now_ms: u64) {
        let job = match self.invoke_pending.get(&req_id).cloned() {
            Some(j) => j,
            None => return, // stale or duplicate completion; ignore
        };

        if !job.retried {
            if let Err(e) = &result {
                if is_invoke_timeout(e) {
                    if let Some(j) = self.invoke_pending.get_mut(&req_id) {
                        j.retried = true;
                    }
                    if let Some(retry) = self.invoke_pending.get(&req_id).cloned() {
                        self.invoker.run(retry); // reuses the in-flight slot
                    }
                    return;
                }
            }
        }

        self.invoke_pending.remove(&req_id);
        // The activity just ended: mark the board dirty so the panel drops the label.
        self.push_board(now_ms, false);
        self.invoke_in_flight = self.invoke_in_flight.saturating_sub(1);
        if let Some(idx) = self.projects.iter().position(|o| o.project.id.0 == job.proj_slug) {
            self.step(
                idx,
                kernel::Input::Command(kernel::Command::InvokeResult {
                    purpose: job.purpose,
                    outcome: result,
                }),
                now_ms,
            );
        }
        self.pump_invoke_queue();
    }

    fn pump_invoke_queue(&mut self) {
        while self.invoke_in_flight < INVOKE_POOL_CAP {
            let req_id = match self.invoke_queue.pop_front() {
                Some(r) => r,
                None => break,
            };
            if let Some(job) = self.invoke_pending.get(&req_id).cloned() {
                self.invoke_in_flight += 1;
                self.invoker.run(job);
            }
        }
    }

    // -- reconciliation (9.1.3 / 9.1.4 / 5.2.4) ----------------------------

    /// One reconcile pass: runtime ceiling first (unconditional force-kill of over-age
    /// bindings, 5.2.4), then poll live bindings, then sweep orphans.
    pub fn reconcile(&mut self, now_ms: u64) {
        // 5.2.4 — runtime ceiling via the kernel's Reconcile input. Done FIRST so an
        // over-age binding is killed + re-queued with no liveness dependency.
        for i in self.owned_dispatchable_indices() {
            self.step(i, kernel::Input::Host(kernel::HostEvent::Reconcile), now_ms);
        }
        self.poll_bindings(now_ms);
        self.orphan_sweep(now_ms);
    }

    /// 9.1.3 — `agents.status` on every remaining live binding. The killed path keys off the
    /// ERROR VALUES `unknown agentId` / `session closed`, NOT off a `status` field; a live
    /// terminal `status` routes to the completion path; `running`/`queued` keep polling.
    fn poll_bindings(&mut self, now_ms: u64) {
        let bindings = self.live_bindings();
        for (idx, agent_id) in bindings {
            let reply = self.host.call("agents.status", json!({ "agentId": agent_id }));
            if is_agent_gone(&reply) {
                self.step(
                    idx,
                    kernel::Input::Host(kernel::HostEvent::AgentsDone {
                        agent_id,
                        status: "killed".to_string(),
                        // The status-poll liveness path carries no koma error text.
                        error: None,
                    }),
                    now_ms,
                );
                continue;
            }
            match reply.get("status").and_then(Value::as_str) {
                Some(s) if is_terminal(s) => {
                    self.step(
                        idx,
                        kernel::Input::Host(kernel::HostEvent::AgentsDone {
                            agent_id,
                            status: s.to_string(),
                            // The status-poll liveness path carries no koma error text.
                            error: None,
                        }),
                        now_ms,
                    );
                }
                _ => {} // running / queued / unknown-non-terminal -> keep polling
            }
        }
    }

    /// 9.1.4 — `agents.list` sweep: any office worker/reviewer we own that no task binding
    /// references (including `status:"gone"` entries) is an orphan from a crash between
    /// spawn and persist; kill it. `status:"gone"` is consumed HERE, from `agents.list`.
    fn orphan_sweep(&mut self, now_ms: u64) {
        let _ = now_ms;
        let reply = self.host.call("agents.list", json!({}));
        let entries = match reply.as_array() {
            Some(a) => a.clone(),
            None => return,
        };
        let tracked = self.tracked_agent_ids();
        for e in entries {
            let agent = e.get("agent").and_then(Value::as_str).unwrap_or("");
            // Worker ids now carry a persona suffix (`office-worker-<name>`), so match the
            // prefix; the fixed-staff ids stay exact.
            let is_office_agent = agent.starts_with("office-worker")
                || agent == "office-reviewer"
                || agent == "office-researcher"
                || agent == "office-auditor";
            if !is_office_agent {
                continue;
            }
            if let Some(id) = parse_agent_id(&e) {
                if !tracked.contains(&id) {
                    self.host.call("agents.kill", json!({ "agentId": id }));
                }
            }
        }
    }

    // -- outbox (6.5) -------------------------------------------------------

    /// Send at most one buffered notice this call, honoring the host's chat.prompt budget:
    /// `{queued}` -> sent; `prompt queue full (5)` -> keep for retry; `turn budget
    /// exhausted` -> pause until the next `agent.turn_end`.
    fn drain_outbox(&mut self, _now_ms: u64) {
        // Find the first sendable notice across owned projects (deterministic order).
        let mut target: Option<(usize, u64, String)> = None;
        for (i, o) in self.projects.iter().enumerate() {
            if o.lease.is_none() {
                continue;
            }
            if let Some(n) = o.project.outbox.iter().find(|n| !n.sent && !n.paused) {
                target = Some((i, n.id, n.text.clone()));
                break;
            }
        }
        let (idx, notice_id, text) = match target {
            Some(t) => t,
            None => return,
        };

        let reply = self.host.call("chat.prompt", json!({ "text": text }));
        let notice = self.projects[idx]
            .project
            .outbox
            .iter_mut()
            .find(|n| n.id == notice_id);
        let notice = match notice {
            Some(n) => n,
            None => return,
        };
        if reply.get("queued").is_some() {
            notice.sent = true;
        } else if let Some(err) = error_str(&reply) {
            if err.contains("turn budget exhausted") {
                notice.paused = true;
            }
            // "prompt queue full (5)" and other transients: leave unsent for next tick.
        }
        let _ = self.store.save_project(&self.projects[idx].project);
    }

    // -- panel + context ----------------------------------------------------

    /// Build and push a full-snapshot envelope (10.2/10.3), applying the 900KB size guard
    /// (drop to summary + `truncated:true`) and the 250ms throttle. A throttled push sets
    /// `push_pending` so the next tick flushes it.
    pub fn push_board(&mut self, now_ms: u64, force: bool) {
        if !force && now_ms.saturating_sub(self.last_push_ms) < PUSH_THROTTLE_MS {
            self.push_pending = true;
            return;
        }
        let projects: Vec<Project> = self.projects.iter().map(|o| o.project.clone()).collect();

        let mut envelope = self.envelope(&projects, SnapshotMode::Full, false);
        if serialized_len(&envelope) > PANEL_PUSH_GUARD {
            envelope = self.envelope(&projects, SnapshotMode::Summary, true);
        }

        self.push_seq += 1;
        self.last_push_ms = now_ms;
        self.push_pending = false;
        self.update_cache(&envelope);
        self.host.panel_push("board", envelope);
    }

    fn envelope(&self, projects: &[Project], mode: SnapshotMode, truncated: bool) -> Value {
        let mut activity: HashMap<String, OfficeActivity> = HashMap::new();
        for o in &self.projects {
            if let Some(a) = office_activity(&self.invoke_pending, &o.project) {
                activity.insert(o.project.id.0.clone(), a);
            }
        }
        json!({
            "kind": "snapshot",
            "seq": self.push_seq,
            "truncated": truncated,
            "projects": panel_snapshot_with_activity(projects, mode, Some(&activity)),
        })
    }

    fn update_cache(&self, envelope: &Value) {
        if let Some(m) = CACHE.get() {
            if let Ok(mut g) = m.lock() {
                g.board = Some(envelope.clone());
            }
        }
    }

    /// Republish the `context.set` board blob (6.6). Byte-capped by the builder; a
    /// `grant denied:` reply is fatal misconfig and surfaces (logged) but never retried.
    fn republish_context(&mut self) {
        let projects: Vec<Project> = self.projects.iter().map(|o| o.project.clone()).collect();
        let text = context_blob(&projects);
        let reply = self.host.call("context.set", json!({ "text": text }));
        if is_grant_denied(&reply) {
            log_line("context.set grant denied; office context not published");
        }
    }

    fn reload_prd_cache(&mut self) {
        for i in 0..self.projects.len() {
            self.sync_prd_cache(i);
        }
    }

    fn sync_prd_cache(&self, idx: usize) {
        let slug = self.projects[idx].project.id.0.clone();
        let prd = self.projects[idx].project.prd_markdown.clone();
        if let Some(m) = CACHE.get() {
            if let Ok(mut g) = m.lock() {
                g.prds.insert(slug, prd);
            }
        }
    }

    // -- desks (7.1) --------------------------------------------------------

    /// Create a task desk directory and stamp the workspace `koma-workflow` marker files.
    fn ensure_desk(&self, dir: &std::path::Path) {
        let _ = std::fs::create_dir_all(dir);
        // Best-effort workspace markers (DO-NOT-ENTER + gitignore "*") at the koma-workflow
        // root, so the whole area is human-marked and never enters the user's VCS (7.1).
        if let Some(ws) = &self.workspace {
            let root = ws.join("koma-workflow");
            let _ = std::fs::create_dir_all(&root);
            let dne = root.join("DO-NOT-ENTER.md");
            if !dne.exists() {
                let _ = std::fs::write(
                    &dne,
                    "# Workflow working area\n\nAgents operate here. Humans: read-only please. Managed by aula.workflow.\n",
                );
            }
            let gi = root.join(".gitignore");
            if !gi.exists() {
                let _ = std::fs::write(&gi, "*\n");
            }
        }
    }

    // -- capacity + lookups -------------------------------------------------

    /// Session-global remaining office slots (5.2.3): the cap minus every in-flight worker
    /// and spawned reviewer across all owned projects.
    fn remaining_capacity(&self) -> u32 {
        let in_flight: u32 = self
            .projects
            .iter()
            .filter(|o| o.lease.is_some())
            .map(|o| project_in_flight(&o.project))
            .sum();
        SESSION_AGENT_CAP.saturating_sub(in_flight)
    }

    fn owned_running_indices(&self) -> Vec<usize> {
        self.projects
            .iter()
            .enumerate()
            .filter(|(_, o)| o.lease.is_some() && matches!(o.project.phase, ProjectPhase::Running))
            .map(|(i, _)| i)
            .collect()
    }

    /// Owned projects the reconcile ceiling should scan: Running or Interrupted (a soft
    /// drain still has live bindings that can age out, 9.1.3), PLUS any project with a live
    /// research binding (6.2b) OR audit binding (6.2c) — a mid-research Drafting project or a
    /// mid-audit completing project must have its analyst's runtime ceiling enforced too, or a
    /// hung researcher/auditor would wedge the pipeline/completion.
    fn owned_dispatchable_indices(&self) -> Vec<usize> {
        self.projects
            .iter()
            .enumerate()
            .filter(|(_, o)| {
                o.lease.is_some()
                    && (matches!(
                        o.project.phase,
                        ProjectPhase::Running | ProjectPhase::Interrupted
                    ) || o.project.research.is_some()
                        || o.project.audit.is_some())
            })
            .map(|(i, _)| i)
            .collect()
    }

    fn owned_project_by_id(&self, id: &str) -> Option<usize> {
        self.projects
            .iter()
            .position(|o| o.lease.is_some() && o.project.id.0 == id)
    }

    fn project_by_task(&self, task: &TaskId) -> Option<usize> {
        self.projects
            .iter()
            .position(|o| o.project.tasks.iter().any(|t| &t.id == task))
    }

    fn owned_project_by_task(&self, task: &TaskId) -> Option<usize> {
        self.projects.iter().position(|o| {
            o.lease.is_some() && o.project.tasks.iter().any(|t| &t.id == task)
        })
    }

    fn owned_project_by_agent(&self, agent_id: u64) -> Option<usize> {
        self.projects.iter().position(|o| {
            o.lease.is_some()
                && (task_bound_to(&o.project, agent_id).is_some()
                    || research_bound_to(&o.project, agent_id)
                    || audit_bound_to(&o.project, agent_id))
        })
    }

    /// All (project idx, agent id) pairs for live real bindings across owned projects —
    /// including the project-level research binding (6.2b) so a killed researcher is caught by
    /// the `agents.status` poll exactly like a killed worker/reviewer.
    fn live_bindings(&self) -> Vec<(usize, u64)> {
        let mut out = Vec::new();
        for (i, o) in self.projects.iter().enumerate() {
            if o.lease.is_none() {
                continue;
            }
            for t in &o.project.tasks {
                if let Some(id) = binding_agent_id(&t.state) {
                    if id != 0 {
                        out.push((i, id));
                    }
                }
            }
            if let Some(id) = research_agent_id(&o.project) {
                out.push((i, id));
            }
            if let Some(id) = audit_agent_id(&o.project) {
                out.push((i, id));
            }
        }
        out
    }

    fn tracked_agent_ids(&self) -> std::collections::HashSet<u64> {
        let mut set = std::collections::HashSet::new();
        for o in &self.projects {
            for t in &o.project.tasks {
                if let Some(id) = binding_agent_id(&t.state) {
                    if id != 0 {
                        set.insert(id);
                    }
                }
            }
            if let Some(id) = research_agent_id(&o.project) {
                set.insert(id);
            }
            if let Some(id) = audit_agent_id(&o.project) {
                set.insert(id);
            }
        }
        set
    }

    // -- test accessors -----------------------------------------------------

    /// Read a project by slug (tests + inline-reply paths).
    pub fn project(&self, slug: &str) -> Option<&Project> {
        self.projects
            .iter()
            .find(|o| o.project.id.0 == slug)
            .map(|o| &o.project)
    }

    /// Whether we currently hold the dispatch lease for `slug` (tests).
    pub fn holds_lease(&self, slug: &str) -> bool {
        self.projects
            .iter()
            .any(|o| o.project.id.0 == slug && o.lease.is_some())
    }

    /// Insert a project directly (tests): persists it and acquires its lease. Mirrors
    /// `bootstrap`'s acquire call, using this daemon's own session — never the project's
    /// `bound_session` (see the comment in `bootstrap`).
    pub fn insert_for_test(&mut self, project: Project, now_ms: u64) {
        let _ = self.store.save_project(&project);
        let slug = project.id.0.clone();
        let path = self.store.lease_path(&slug);
        let lease = lease::acquire(&path, &self.instance, self.session.as_deref(), self.pid, now_ms)
            .ok()
            .flatten();
        self.projects.push(Owned { project, lease });
    }

    /// Snapshot of all loaded projects (tests).
    pub fn projects_for_test(&self) -> Vec<Project> {
        self.projects.iter().map(|o| o.project.clone()).collect()
    }
}

// ---------------------------------------------------------------------------
// Heartbeat (own thread, 4.4 / 5.1)
// ---------------------------------------------------------------------------

/// Refresh every lease this instance holds. Runs on a DEDICATED thread so a slow host call
/// on the driver can never age a lease past the 60s steal window (5.6). Makes no host calls.
pub fn heartbeat_owned(store: &Store, instance: &str, now_ms: u64) {
    let rows = match store.registry() {
        Ok(r) => r,
        Err(_) => return,
    };
    for row in rows {
        let path = store.lease_path(&row.project_id);
        if let Ok(Some(l)) = lease::read(&path) {
            if l.instance == instance {
                let _ = lease::heartbeat(&path, &l, now_ms);
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Free helpers
// ---------------------------------------------------------------------------

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

/// Derives the live "office activity" for a project, if any. Priority when multiple sources
/// are live: a pending invoke wins over research, which wins over audit (an invoke result
/// often clears research/audit synchronously, so this keeps the label from flickering).
fn office_activity(pending: &HashMap<u64, InvokeJob>, project: &Project) -> Option<OfficeActivity> {
    if let Some(job) = pending.values().find(|j| j.proj_slug == project.id.0) {
        let label = match job.purpose {
            InvokePurpose::Persona => "office is replying",
            InvokePurpose::Fold => "summarizing the conversation",
            InvokePurpose::AssumeCheckPrd => "fact-checking the PRD",
            InvokePurpose::AssumeCheckTrd => "fact-checking the TRD",
            InvokePurpose::AssumeCheckCrd => "fact-checking the CRD",
            InvokePurpose::Trd => "drafting the TRD",
            InvokePurpose::Crd => "drafting the CRD",
            InvokePurpose::Breakdown | InvokePurpose::BreakdownReask | InvokePurpose::BreakdownCompact => {
                "breaking down the plan"
            }
        };
        return Some(OfficeActivity { label: label.to_string(), since_ms: job.submitted_at_ms });
    }
    if let Some(b) = &project.research {
        return Some(OfficeActivity { label: "researching the stack".to_string(), since_ms: b.spawned_at_ms });
    }
    if let Some(b) = &project.audit {
        return Some(OfficeActivity { label: "auditing the delivery".to_string(), since_ms: b.spawned_at_ms });
    }
    None
}

fn binding_agent_id(state: &TaskState) -> Option<u64> {
    match state {
        TaskState::OnProgress { binding, .. } => Some(binding.ext_agent_id),
        TaskState::Review { binding: Some(b), .. } => Some(b.ext_agent_id),
        _ => None,
    }
}

/// The real (non-provisional) research agent id for a project, if one is in flight (6.2b).
fn research_agent_id(p: &Project) -> Option<u64> {
    match &p.research {
        Some(b) if b.ext_agent_id != 0 => Some(b.ext_agent_id),
        _ => None,
    }
}

/// Whether `agent_id` is a project's live (non-provisional) research binding (6.2b).
fn research_bound_to(p: &Project, agent_id: u64) -> bool {
    matches!(&p.research, Some(b) if b.ext_agent_id == agent_id && b.ext_agent_id != 0)
}

/// The real (non-provisional) audit agent id for a project, if one is in flight (6.2c).
fn audit_agent_id(p: &Project) -> Option<u64> {
    match &p.audit {
        Some(b) if b.ext_agent_id != 0 => Some(b.ext_agent_id),
        _ => None,
    }
}

/// Whether `agent_id` is a project's live (non-provisional) audit binding (6.2c).
fn audit_bound_to(p: &Project, agent_id: u64) -> bool {
    matches!(&p.audit, Some(b) if b.ext_agent_id == agent_id && b.ext_agent_id != 0)
}

fn task_bound_to(p: &Project, agent_id: u64) -> Option<&office_core::Task> {
    p.tasks.iter().find(|t| match &t.state {
        TaskState::OnProgress { binding, .. } => binding.ext_agent_id == agent_id,
        TaskState::Review { binding: Some(b), .. } => b.ext_agent_id == agent_id,
        _ => false,
    })
}

/// The two `agents.status` error VALUES that mean "agent is gone" (5.2.1). The killed path
/// keys off these strings, never off a `status` field (`status:"gone"` is `agents.list`-only).
fn is_agent_gone(reply: &Value) -> bool {
    match error_str(reply) {
        Some(s) => s.starts_with("unknown agentId") || s.starts_with("session closed"),
        None => false,
    }
}

fn is_terminal(status: &str) -> bool {
    matches!(status, "done" | "error" | "killed")
}

/// Whether an invoke error string is the host's model-call timeout (`model call timed
/// out`; broker inner 330s, wire cap 360s) — the one class the driver retries exactly once (5.1 / 6.2).
fn is_invoke_timeout(err: &str) -> bool {
    err.contains("timed out") || err.contains("timeout")
}

fn error_str(v: &Value) -> Option<&str> {
    v.get("error").and_then(Value::as_str)
}

/// Parse an ext-facing agent id from a reply, tolerating both `u64` and numeric-string forms
/// (host mode is `u64`; `sessions.list`/demo can render ids as strings).
/// Name a project minted implicitly by a brief that arrived without a project id:
/// the first few words of the brief message, capped, so the dashboard row reads like
/// a project and not a paragraph. Empty/whitespace briefs fall back to "untitled".
pub(crate) fn derive_project_name(message: &str) -> String {
    let name: String = message.split_whitespace().take(6).collect::<Vec<_>>().join(" ");
    let name = name.trim().to_string();
    if name.is_empty() {
        return "untitled".to_string();
    }
    if name.len() > 48 {
        let mut cut = 48;
        while !name.is_char_boundary(cut) {
            cut -= 1;
        }
        name[..cut].trim_end().to_string()
    } else {
        name
    }
}

fn parse_agent_id(v: &Value) -> Option<u64> {
    match v.get("agentId") {
        Some(Value::Number(n)) => n.as_u64(),
        Some(Value::String(s)) => s.parse::<u64>().ok(),
        _ => None,
    }
}

fn first_session_id(sessions: &Value) -> Option<String> {
    sessions
        .as_array()?
        .iter()
        .find_map(|s| s.get("id").and_then(Value::as_str).map(str::to_string))
}

fn first_session_workdir(sessions: &Value) -> Option<String> {
    sessions
        .as_array()?
        .iter()
        .find_map(|s| s.get("workdir").and_then(Value::as_str).map(str::to_string))
}

fn serialized_len(v: &Value) -> usize {
    serde_json::to_vec(v).map(|b| b.len()).unwrap_or(0)
}

/// Runtime logging goes to `~/.koma-workflow` stderr sink; a daemon must never smear the
/// host TUI frame, but stderr is safe for the extension process (it is not the TUI owner).
fn log_line(msg: &str) {
    eprintln!("workflow-driver: {msg}");
}

#[cfg(test)]
#[path = "driver_test.rs"]
mod driver_test;
