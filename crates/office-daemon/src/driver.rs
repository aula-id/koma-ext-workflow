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
//! `Koma::call` blocks up to 120s and `models.invoke` up to 25s; neither may stall
//! the tick loop. The lease heartbeat runs on its OWN thread ([`heartbeat_owned`]) so
//! a slow host call can never age the lease past the 60s steal window (4.4, 5.6), and
//! `InvokeModel` effects are handed to a worker pool in W9 (the kernel emits none in
//! W7, so the effect arm here is a documented no-op).

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Mutex, OnceLock};

use office_core::digest::{context_blob, panel_snapshot};
use office_core::{
    kernel, CommentAuthor, Effect, Project, ProjectPhase, SnapshotMode, TaskId, TaskState,
};
use office_store::{lease, Lease, Store};
use serde_json::{json, Value};

use crate::handlers::{Command as DCmd, HostEvent as DEvt, Input as DInput};
use crate::host::{is_grant_denied, Host};

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
        })
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
        for i in 0..self.projects.len() {
            let slug = self.projects[i].project.id.0.clone();
            let bound = self.projects[i].project.bound_session.clone();
            let path = self.store.lease_path(&slug);
            let lease = lease::acquire(&path, &self.instance, bound.as_deref(), self.pid, now_ms)
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

    /// One `recv_timeout` tick: rate-limited reconcile, then a dispatch scan for every
    /// owned Running project, then one outbox drain.
    pub fn on_tick(&mut self, now_ms: u64) {
        if now_ms.saturating_sub(self.last_reconcile_ms) >= RECONCILE_MS {
            self.reconcile(now_ms);
            self.last_reconcile_ms = now_ms;
        }
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
            // W8/W9 surface (office persona, PRD/breakdown/authorize, project lifecycle,
            // board edits). Recognized here so the driver never drops them silently once
            // those waves wire them; no-ops for now.
            DCmd::Brief { .. }
            | DCmd::Status { .. }
            | DCmd::Authorize { .. }
            | DCmd::Projects
            | DCmd::OfficeChat { .. }
            | DCmd::CardMove { .. }
            | DCmd::EditTask { .. }
            | DCmd::EditDeps { .. }
            | DCmd::ConfigSet { .. }
            | DCmd::ProjectCreate { .. }
            | DCmd::ProjectArchive { .. }
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

    // -- event routing ------------------------------------------------------

    fn handle_event(&mut self, e: DEvt, now_ms: u64) {
        match e {
            // The private notify from `agents.spawn { notify: true }` — the ONLY event
            // carrying our ext-facing agent id (2.2). Correlate to the owning project.
            DEvt::AgentsDone { agent_id, status } => {
                if let Some(i) = self.owned_project_by_agent(agent_id) {
                    self.step(
                        i,
                        kernel::Input::Host(kernel::HostEvent::AgentsDone { agent_id, status }),
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
                Effect::Kill { ext_agent_id } => {
                    self.host.call("agents.kill", json!({ "agentId": ext_agent_id }));
                }
                Effect::FetchResult { ext_agent_id } => self.fetch_result(idx, ext_agent_id, now_ms),
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
                // Wired in W9 onto the off-loop invoke pool; the W7 kernel emits none.
                Effect::InvokeModel { .. } => {}
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
        agent: &'static str,
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
            if agent != "office-worker" && agent != "office-reviewer" {
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
        json!({
            "kind": "snapshot",
            "seq": self.push_seq,
            "truncated": truncated,
            "projects": panel_snapshot(projects, mode),
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
    /// drain still has live bindings that can age out, 9.1.3).
    fn owned_dispatchable_indices(&self) -> Vec<usize> {
        self.projects
            .iter()
            .enumerate()
            .filter(|(_, o)| {
                o.lease.is_some()
                    && matches!(
                        o.project.phase,
                        ProjectPhase::Running | ProjectPhase::Interrupted
                    )
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
        self.projects
            .iter()
            .position(|o| o.lease.is_some() && task_bound_to(&o.project, agent_id).is_some())
    }

    /// All (project idx, agent id) pairs for live real bindings across owned projects.
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

    /// Insert a project directly (tests): persists it and acquires its lease.
    pub fn insert_for_test(&mut self, project: Project, now_ms: u64) {
        let _ = self.store.save_project(&project);
        let slug = project.id.0.clone();
        let bound = project.bound_session.clone();
        let path = self.store.lease_path(&slug);
        let lease = lease::acquire(&path, &self.instance, bound.as_deref(), self.pid, now_ms)
            .ok()
            .flatten();
        self.projects.push(Owned { project, lease });
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

fn binding_agent_id(state: &TaskState) -> Option<u64> {
    match state {
        TaskState::OnProgress { binding, .. } => Some(binding.ext_agent_id),
        TaskState::Review { binding: Some(b), .. } => Some(b.ext_agent_id),
        _ => None,
    }
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

fn error_str(v: &Value) -> Option<&str> {
    v.get("error").and_then(Value::as_str)
}

/// Parse an ext-facing agent id from a reply, tolerating both `u64` and numeric-string forms
/// (host mode is `u64`; `sessions.list`/demo can render ids as strings).
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
