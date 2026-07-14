# Workflow — Architecture

A koma extension that runs a VIRTUAL OFFICE: an autonomous software production line staffed
by koma sub-agents, coordinated by a deterministic Rust kernel, fronted by an LLM "front
office" persona and a React dashboard panel.

Host target: koma main @6f4f0d2 (`/media/wangsa/project-x/simple-coders`, READ-ONLY).
The extension consumes the merged host API as-is. Every host-API claim below carries a
`file:line` reference into that tree. Zero host modifications.

Design principles (locked):

- NO LLM in the control loop. The kernel is deterministic Rust. The office persona and
  the worker/reviewer sub-agents are the only token spenders.
- Everything resumable. Durable state survives extension restart AND koma restart.
- Respect every host cap: context.set 8KB, chat.prompt 16KB + queue 5 + turn budget 10,
  models.invoke 32KB/25s, panel push 1MiB, panel->host 256KiB, wire frame 4MiB fatal.
- No emoji anywhere. Tests in `*_test.rs` files.

---

## 1. Crate / workspace layout

```
/media/wangsa/project-x/agentic-kanban/
  Cargo.toml                  # [workspace] members = crates/*
  manifest.json               # koma-extension/v0 manifest (single source of truth)
  pack.sh                     # build release binary + Vite UI -> dist/workflow.zip
  dev-install.sh              # local dev install (see 12.7)
  docs/
    ARCHITECTURE.md           # this file
    BUILD_WAVES.md
  crates/
    office-core/              # PURE domain + kernel. No IO, no SDK, no threads.
      src/lib.rs
      src/domain.rs           # Project/Epic/Story/Task/Comment/Prd structs + enums
      src/domain_test.rs
      src/machine.rs          # task + project state machines (pure transitions)
      src/machine_test.rs
      src/graph.rs            # blocked-by DAG: validation, ready-set, halt detection
      src/graph_test.rs
      src/kernel.rs           # deterministic dispatch: Command + HostEvent -> Effect[]
      src/kernel_test.rs
      src/prompts.rs          # worker/reviewer/office prompt builders (pure string fns)
      src/prompts_test.rs
      src/report.rs           # OFFICE-REPORT / OFFICE-REVIEW trailer parser
      src/report_test.rs
      src/digest.rs           # context blob + panel snapshot digests, size-capped
      src/digest_test.rs
    office-store/             # persistence: atomic JSON store + journal + lease
      src/lib.rs
      src/store.rs
      src/store_test.rs
      src/lease.rs
      src/lease_test.rs
    office-daemon/            # the shipped binary (kind: daemon). SDK glue only.
      src/main.rs             # run_daemon entry, OnceLock plumbing
      src/handlers.rs         # on_invoke/on_event -> mpsc commands (never Koma::call)
      src/handlers_test.rs
      src/driver.rs           # driver thread: kernel tick loop, owns the Koma handle
      src/host.rs             # trait Host { call(...) } -> real Koma impl + FakeHost
      src/host_test.rs
      src/inbox.rs            # workspace file-inbox watcher (daemon-mode bridge, 6.4)
      src/inbox_test.rs
  ui/                         # Vite + React 19 + TS + Tailwind + framer-motion
    index.html
    vite.config.ts            # base: './'  (MANDATORY, see 10.1)
    src/...
    public/koma-panel.js      # copied verbatim from src-extension sample
```

SDK dependency (path, not published — src-extension/README.md:184):

```toml
# crates/office-daemon/Cargo.toml
[dependencies]
koma-extension = { path = "/media/wangsa/project-x/simple-coders/src-extension" }
serde = { version = "1", features = ["derive"] }
serde_json = "1"
```

`office-core` and `office-store` depend only on serde/serde_json (plus `tempfile` as a
dev-dependency for store tests). This keeps the kernel testable without any host.

The SDK is v0 "unstable, will break" — the path dep pins us to the local checkout, which
is exactly the contract we recon'd. `manifest.json` is loaded with
`include_str!("../../../manifest.json")` and parsed as `ExtensionManifest` at startup so
schema drift fails loudly (same pattern as the host samples).

### manifest.json

```json
{
  "schema": "koma-extension/v0",
  "id": "aula.workflow",
  "name": "Workflow",
  "version": "0.1.0",
  "description": "Autonomous kanban office: PRD front desk, sub-agent workforce, review line, live dashboard.",
  "tier": "free",
  "kind": "daemon",
  "runtime": { "exec": "bin/office-daemon", "args": [] },
  "requires": [
    "agents:orchestrate",
    "sessions:manage",
    "chat:prompt",
    "models:invoke",
    "context:publish"
  ],
  "contributes": {
    "panels": [ { "id": "board", "title": "Workflow", "icon": "kanban" } ],
    "events": [ "subagent.done", "agent.turn_end", "session.foreground_change" ],
    "sub_agents": [
      { "name": "office-worker",
        "description": "Workflow task implementer. Executes one kanban task inside its desk directory and files an OFFICE-REPORT.",
        "prompt": "<worker persona system prompt, see 8.1>" },
      { "name": "office-reviewer",
        "description": "Workflow reviewer. Verifies one task against its acceptance criteria and files an OFFICE-REVIEW verdict.",
        "prompt": "<reviewer persona system prompt, see 8.2>" }
    ],
    "tools": [
      { "name": "workflow_brief",     "description": "Talk to the Workflow front desk (requirements, PRD, negotiation). Input: message.", "input_schema": { "type": "object", "properties": { "message": { "type": "string" } }, "required": ["message"] } },
      { "name": "workflow_status",    "description": "Board digest for one project or all projects.", "input_schema": { "type": "object", "properties": { "project": { "type": "string" } } } },
      { "name": "workflow_authorize", "description": "Authorize a project to start grinding. Requires delivery_path.", "input_schema": { "type": "object", "properties": { "project": { "type": "string" }, "delivery_path": { "type": "string" } }, "required": ["project", "delivery_path"] } },
      { "name": "workflow_interrupt", "description": "Interrupt a running project (mode: soft|hard, default hard).", "input_schema": { "type": "object", "properties": { "project": { "type": "string" }, "mode": { "type": "string" } }, "required": ["project"] } },
      { "name": "workflow_resume",    "description": "Resume an interrupted or halted project.", "input_schema": { "type": "object", "properties": { "project": { "type": "string" } }, "required": ["project"] } },
      { "name": "workflow_comment",   "description": "Attach a comment to a task card; the task's agent will consume it.", "input_schema": { "type": "object", "properties": { "task": { "type": "string" }, "text": { "type": "string" } }, "required": ["task", "text"] } },
      { "name": "workflow_projects",  "description": "List office projects with phase and progress.", "input_schema": { "type": "object" } }
    ]
  }
}
```

Notes, all backed by recon:

- Grant set is minimal-sufficient. `agents:orchestrate` implies `agents:read`
  (broker.rs:338-369, the only lattice edge). We do NOT request `models:contribute` or
  `oauth:contribute` — the office is model-agnostic and registers nothing.
- Sub-agent defs omit `model` entirely: there is no "inherit" literal; `model: None`
  falls through the resolution chain to `resolve_role(Main)` (EXTENSIONS.md:324-353,
  resolve.rs). User rebinding is done at spawn time via the `model` param (see 5.4).
- `contributes.events` is subscription-only, no grant needed. `agents.done` is NOT
  listed because it is not subscription-gated — it is armed by `agents.spawn
  { notify: true }` (events.rs:88-102, verified emit site).
- One panel only: the GUI can launch only `panels[0]` (ActivityBar.tsx:78-91); the
  dashboard is a single SPA with internal routing.

---

## 2. Where the extension actually runs (deployment model)

Verified host behavior that shapes everything:

- Every koma daemon boot iterates `installed_extensions` and starts every enabled
  daemon-kind extension (lifecycle/mod.rs:241-279). Under daemon-per-session, EACH
  session daemon runs its OWN instance of office-daemon.
- `agents.spawn` targets that daemon's foreground session (broker.rs:564-635);
  `sessions.spawn_into` with a session uuid live in the same daemon is local and
  TRACKED; any other uuid is cross-process fire-and-forget, untrackable
  (broker.rs:1809-1871).
- Sub-agents run with workspace = the session's effective cwd (stream/spawn.rs:23) and
  cannot `cd` (subagent/engine.rs:431-436). File tools resolve paths against the session
  workdir allow-list and REJECT absolute paths outside it, except `/tmp/koma` scratch
  (tool/mod.rs:283-330, verified).

Consequences (locked decisions):

1. **A project is bound to one session** (`bound_session` uuid + its workdir). The
   instance of office-daemon living in that session's daemon is the project's OWNER and
   the only instance that dispatches for it. Ownership is enforced by a lease file
   (see 4.4), because every session daemon runs a copy of us against the same durable
   state root.
2. **Local session discovery**: the extension reads its own session(s) directly from
   `sessions.list` (broker.rs:1639-1665), which returns each live session's `{ id, workdir }`.
   Under daemon-per-session this is exactly one row — this daemon's own session — so binding
   is unambiguous: `bound_session = sessions.list()[0].id` and `workspace = its workdir`.
   Event payloads (`agent.turn_end { session }`, `session.foreground_change { session }`,
   `subagent.done { session, ... }`, EXTENSIONS.md:548-552) are kept only as a redundant
   corroboration channel, never as the primary correlation: `subagent.done.subagentId` is a
   per-session LOCAL id, not the ext-facing `agentId` that `agents.spawn` returned
   (events.rs:70-115), and the private `agents.done` callback carries no `session` field, so
   NO event ties a specific spawn to a session uuid — we do not attempt that correlation. In a
   multi-session TUI daemon `sessions.list` returns several rows; the office then picks the
   session whose `workdir` contains the project's delivery path, falling back to the
   most-recently-foregrounded session (from `session.foreground_change`) as a best-effort
   heuristic that the user can override in the panel. After binding, all spawns use
   `sessions.spawn_into { session: bound }` for a deterministic target (recon gotcha:
   foreground can move under `agents.spawn` in multi-session TUI daemons).
3. **Delivery path and desks MUST live inside the bound session's workspace** so worker
   file tools can touch them (containment verified at tool/mod.rs:283). Authorization
   validates this against the session's `workdir` from `sessions.list`
   (broker.rs:1639-1665). See 7 and Limitations 13.5.

---

## 3. Domain model (`office-core/src/domain.rs`)

All ids are short human-readable slugs, unique within their parent, plus a monotonic
numeric suffix minted by the store (`auth-login-3`). No random uuids in names.

```rust
pub struct ProjectId(pub String);   // "shop-crawler"
pub struct EpicId(pub String);      // "shop-crawler/e1-ingest"
pub struct StoryId(pub String);     // "shop-crawler/e1-ingest/s2-parser"
pub struct TaskId(pub String);      // "shop-crawler/e1-ingest/s2-parser/t4-retry-logic"
pub struct CommentId(pub u64);      // per-project monotonic
pub struct AgentBinding { pub ext_agent_id: u64, pub session: String, pub spawned_at_ms: u64,
                          pub kind: AgentKind /* Worker | Reviewer */ }

pub enum Column { Backlog, Todo, OnProgress, Review, Done }

pub enum TaskState {
    Backlog,                       // known, not yet groomed into the line
    Todo,                          // ready to be picked when deps are Done
    OnProgress { binding: AgentBinding, attempt: u32 },
    Review     { binding: Option<AgentBinding>, attempt: u32 }, // None = reviewer not yet spawned
    Parked     { reason: ParkReason, attempt: u32 },            // escalated out of the line
    Done       { at_ms: u64 },
}
// Column is a projection of TaskState: Parked renders in the Review column with a
// "parked" badge (the five columns are the locked vocabulary; Parked is a flag, not
// a sixth column).

pub enum ParkReason { ReviewBounceBudget, WorkerBlocked(String), SpawnFailed(String) }

pub struct Task {
    pub id: TaskId, pub title: String, pub description: String,
    pub acceptance: Vec<String>,          // done-criteria, reviewer checks these
    pub blocked_by: Vec<TaskId>,          // DAG edges, validated acyclic on every mutation
    pub priority: i32,                    // higher first; ties by id (deterministic)
    pub state: TaskState,
    pub bounces: u32,                     // failed reviews so far
    pub comments: Vec<Comment>,
    pub desk: Option<PathBuf>,            // absolute desk dir once created
    pub last_report: Option<String>,      // worker report text (truncated to 64KB)
    pub last_review: Option<String>,      // reviewer verdict text (truncated to 64KB)
    pub history: Vec<TaskEvent>,          // audit trail for the panel
}

pub struct Comment {
    pub id: CommentId, pub author: CommentAuthor, // User | Office | System
    pub text: String, pub created_ms: u64,
    pub receipt: Receipt,
}
pub enum Receipt {
    Pending,                    // created, no agent has ever received it
    Delivered { at_ms: u64 },   // included in a spawned agent's prompt (agent may not have read it)
    Read      { at_ms: u64 },   // agent ACKed it via the ack-comments trailer; REQUIRES a prior Delivered
}
// A receipt only advances Pending -> Delivered -> Read, and Read is reachable ONLY through
// an explicit ack (never inferred from task completion). Rationale: the receipt exists so the
// user can trust that the agent actually saw the comment. A comment added while a task is
// OnProgress cannot reach the running worker (Limitation 2) and stays Pending; if that worker
// finishes and the task passes review on the first try (no re-spawn), the comment was never
// placed in any prompt, so it MUST remain Pending — asserting Read there would be a false
// receipt. Comments still Pending when a task reaches Done surface in the panel as
// "unread / never delivered" so the user knows to re-file or reopen (5.3, task-detail view 10.3).

pub struct Story { pub id: StoryId, pub title: String, pub intent: String, pub tasks: Vec<TaskId> }
pub struct Epic  { pub id: EpicId,  pub title: String, pub intent: String, pub stories: Vec<StoryId> }

pub enum ProjectPhase {
    Drafting,      // front-office PRD discussion; no dispatch
    Ready,         // PRD + breakdown accepted, awaiting authorization (delivery path!)
    Running,       // line is dispatching
    Interrupted,   // user pressed the brake; resumable
    Halted { reason: String },  // line stopped itself (parked task blocks everything)
    Done   { at_ms: u64 },
}

pub struct Project {
    pub id: ProjectId, pub name: String,
    pub phase: ProjectPhase,
    pub prd_markdown: String,             // authored by the office persona
    pub office_transcript: Vec<ChatMsg>,  // front-office conversation (rolling, see 6.2)
    pub office_summary: String,           // rolling summary when transcript folds
    pub delivery_path: Option<PathBuf>,   // HARD REQUIRED before Running
    pub bound_session: Option<String>,    // session uuid that owns dispatch
    pub workspace: Option<PathBuf>,       // bound session workdir (from sessions.list)
    pub epics: Vec<Epic>, pub stories: Vec<Story>, pub tasks: Vec<Task>,
    pub config: ProjectConfig,            // caps, bounce budget, model bindings
    pub outbox: Vec<OutboundNotice>,      // pending chat.prompt notices (durable, see 6.5)
    pub seq: u64,                         // state version, bumped on every mutation
}

pub struct ProjectConfig {
    pub max_workers: u32,        // default 2, clamp 1..=4 (host cap 5 shared with user)
    pub bounce_budget: u32,      // default 3 failed reviews before escalation
    pub worker_model: Option<String>,    // slug override or None = inherit Main
    pub reviewer_model: Option<String>,  // idem
    pub office_role: String,             // models.invoke role, default "main"
    pub worker_max_runtime_ms: u64,      // HARD wall-clock ceiling per spawn, default 20 min.
                                         // The kernel force-kills any binding older than this
                                         // regardless of liveness (5.2.4) — the ONLY backstop that
                                         // bounds a runaway worker's token burn, because the host
                                         // cannot cap ext sub-agent iterations (see 5.2.4).
}
```

### State machines

Task (pure fn `machine::step_task`, exhaustively tested):

```
Backlog -> Todo                      (groom / office breakdown accepted)
Todo -> OnProgress                   (kernel dispatch: deps Done, slot free)
OnProgress -> Review                 (agents.done status=done, report parsed)
OnProgress -> Todo                   (agents.done status=error|killed, or interrupt; attempt kept)
OnProgress -> Parked(WorkerBlocked)  (report says status: blocked)
Review -> Done                       (reviewer verdict pass)
Review -> Todo                       (verdict fail, bounces+1 <= budget; review notes attached)
Review -> Parked(ReviewBounceBudget) (bounces+1 > budget)
Parked -> Todo                       (user un-parks from panel / workflow_resume)
any non-Done -> Todo                 (hard interrupt normalizes in-flight states)
```

Project:

```
Drafting -> Ready        (office breakdown accepted by user)
Ready -> Running         (workflow_authorize with valid delivery_path)
Running -> Interrupted   (interrupt button / workflow_interrupt)
Interrupted -> Running   (resume)
Running -> Halted        (kernel: unfinished tasks exist, none ready, none running,
                          and every unfinished task transitively blocked by a Parked task)
Halted -> Running        (user un-parks or edits the DAG, then resume)
Running -> Done          (all tasks Done) -> chat.prompt "project done" notice
```

Illegal transitions return `Err(Transition)` and are surfaced to the panel; the kernel
never panics on user input.

---

## 4. Persistence (`office-store`)

### 4.1 Location

`~/.koma-workflow/` — deliberately OUTSIDE `~/.koma/extensions/<id>/`, because the install
dir is `remove_dir_all`'d on both reinstall/upgrade (install.rs:151-153) and uninstall
(EXTENSIONS.md:749-770). There is no host-provided data dir convention (verified recon
open question; store.rs has none). The root is announced in the panel and in
`DO-NOT-ENTER.md` so humans know what it is. Layout:

```
~/.koma-workflow/
  README.md                        # what this is, which extension owns it
  registry.json                    # [ { project_id, name, phase, state_dir } ]
  projects/
    <project-slug>/
      state.json                   # full Project (schema-versioned)
      prd.md                       # PRD markdown mirror (human-readable convenience copy)
      journal.ndjson               # append-only audit log (one JSON event per line)
      lease.json                   # dispatch ownership (4.4)
```

### 4.2 Atomic writes, versioned schema

- Every `state.json` write: serialize -> write `state.json.tmp` -> `fsync` -> atomic
  `rename` (and `fsync` the containing dir after rename so a power-loss, not just a process
  restart, cannot lose the rename). `registry.json` same. Journal is append-only with
  line-buffered writes; a torn final line is tolerated by the reader (skip malformed tail).
- **Cross-file write order** (each file is individually atomic, but a crash can land between
  two files): project create/archive mutates BOTH `projects/<slug>/state.json` AND
  `registry.json`. Rule: on create, write `state.json` FIRST, then add the registry row — the
  registry never references a project whose state did not land. On archive, remove the registry
  row FIRST, then delete the state dir. A crash mid-pair therefore leaves at most a state dir
  with no registry row (adoptable), never a registry row pointing at a missing state.
- **Tolerant load / reconciliation** (startup, 9.1 step 1): after loading `registry.json`, the
  driver scans `projects/*/state.json`: a valid `state.json` NOT in the registry is ADOPTED
  (registry row rebuilt from it); a registry row whose `state.json` is missing or corrupt is
  DROPPED (row removed, dir quarantined to `projects/.quarantine/` with a note in the panel).
  The store is thus self-healing against a torn create/archive pair.
- Every file carries `"schema": "workflow/1"`. Loaders refuse newer majors, migrate
  older ones (v1 has a no-op migration table ready).
- `state.json` is the source of truth; `journal.ndjson` is for audit/panel history and is
  never required for recovery.
- Write policy: the driver persists after every kernel transaction that dirtied state
  (spawn recorded, done processed, comment added...). State is small (tens of KB); the
  64KB report truncation keeps it bounded.

### 4.3 What survives what

| Event | Durable state | In-flight agents | context.set blob | chat.prompt buffer |
|---|---|---|---|---|
| extension crash/restart | kept (disk) | keep running in koma; reconciled on start (9.1) | kept host-side until daemon restart, republished anyway | kept host-side |
| koma daemon restart | kept (disk) | DEAD (sub-agents live in the daemon); ExtAgentRegistry cleared | LOST (rest.rs:461, memory-only — verified) | LOST (memory-only) |
| reinstall/upgrade | kept (outside install dir) | n/a | republished on start | outbox re-sent |
| uninstall | kept on disk (documented; user deletes `~/.koma-workflow` manually) | killed with daemon | purged by host | purged by host |

On EVERY start the driver: loads registry + projects, republishes the context blob
(8KB cap), runs agent reconciliation (9.1), resumes dispatch for `Running` projects that
this instance can lease.

### 4.4 Lease (two sessions racing)

Every session daemon runs an office-daemon instance against the same `~/.koma-workflow`.
Only the lease holder dispatches for a project:

- `lease.json` = `{ instance: <uuid4 minted per process>, session: <bound uuid or null>,
  pid, heartbeat_ms }`, rewritten atomically every 10s by the holder. The heartbeat runs on a
  DEDICATED thread (5.1), never interleaved with the driver's blocking effect execution, so a
  slow/hung `Koma::call` (up to 120s) can never age the lease past the 60s steal window — the
  lease only goes stale when the owning process is genuinely dead or wedged.
- Acquire: no file, or heartbeat older than 60s, or `session == our local session`
  (rebind after koma restart). A stale-lease steal additionally checks, via `sessions.list`,
  that the project's `bound_session` is live-and-local to this daemon before dispatching; if it
  is not, `spawn_into` would be cross-process fire-and-forget, so the stealer releases rather
  than firing untracked duplicate workers (5.6). Otherwise the instance treats the project as
  READ-ONLY: panel shows it, mutating panel actions are rejected with "owned by another session".
- All writers (even read-only instances persisting comments) go through an advisory
  `flock` on `state.json.lock` around load-mutate-store, so cross-instance comment adds
  cannot torn-write. Comments are the ONLY mutation a non-holder may perform.

---

## 5. The KERNEL (`office-core/src/kernel.rs` + `office-daemon/src/driver.rs`)

### 5.1 Shape: pure core, thin driver

`office-core::kernel` is a pure function:

```rust
pub enum Input {
    Command(Command),        // panel action, contributed tool call, inbox file, office decision
    Host(HostEvent),         // AgentsDone { agent_id, status }, Tick { now_ms }, ...
}
pub enum Effect {
    Spawn { task: TaskId, prompt: String, agent: &'static str, model: Option<String> },
    Kill { ext_agent_id: u64 },
    FetchResult { ext_agent_id: u64 },      // driver calls agents.result, feeds back Input
    InvokeModel { req_id: u64, role: String, system: String, prompt: String },
                                            // office persona / breakdown / fold; runs OFF the
                                            // driver on an invoke worker thread (see below)
    PublishContext { text: String },        // <= 8KB, built by digest.rs
    QueueChatPrompt { notice_id: u64, text: String },
    PanelPush { snapshot: bool },
    EnsureDesk { task: TaskId, dir: PathBuf },
    Persist,
}
pub fn step(p: &mut Project, input: Input, now_ms: u64) -> Vec<Effect>;
```

The driver thread (the ONLY holder of the real `Koma` handle for the tick loop, per the
deadlock rule sdk.rs:22-33 — `Koma::call` from `on_invoke`/`on_event` deadlocks the
single-threaded serve loop) loops on `mpsc::Receiver<Input>` with `recv_timeout(1s)`; timeout
produces `Tick`. Effects are executed with the host trait:

```rust
pub trait Host { fn call(&mut self, method: &str, params: Value) -> Value; ... }
```

implemented by `Koma` in production and `FakeHost` (scripted replies) in
`driver`/`host` tests. `DaemonDemo::driver` is a bare `fn(&mut Koma)` (sdk.rs:505-508),
so the receiver and shared state are parked in `static OnceLock`s (fleet-board-daemon
reference pattern). `on_invoke`/`on_event` only push onto the sender and (for panel
rehydrate) use a `try_clone`'d handle for write-only `panel_push` — handler-safe per
sdk.rs:220-239.

**Blocking calls must not run inline on the tick loop.** `Koma::call` blocks up to 120s
(sdk.rs:560) and `models.invoke` up to its 25s internal budget (broker.rs:1026); a single
slow call parked in the tick loop would stall dispatch, the 30s reconcile, the 10s lease
heartbeat, AND every 100ms panel-read oneshot (section 11). The driver therefore owns two
extra `try_clone`'d handles (sdk.rs:220 — the SDK primitive we already use for `panel_push`)
and offloads the two dangerous classes of call:

- **Invoke worker pool**: an `InvokeModel` effect is NOT executed inline. The driver dispatches
  it to a small bounded worker pool (default 2 concurrent, capped) that each own a `try_clone`'d
  `Koma`; the worker runs the 25s `models.invoke` and feeds the result back to the kernel as a
  `Command::InvokeResult { req_id, ... }` on `CMD_TX`. The tick loop, reconcile, and panel-read
  oneshots stay responsive during the 25-125s of a multi-invoke PRD authoring flow (6.2-6.3).
  This is what "NO LLM in the control loop" (design principle) means concretely: the kernel
  emits invoke *requests* and consumes invoke *results* as ordinary commands; it never blocks
  on a model. Nothing is lost if the driver is mid-tick when a result arrives — it buffers on
  the mpsc.
- **Lease heartbeat thread**: the 10s lease heartbeat (4.4) runs on its OWN dedicated thread
  with a `try_clone`'d handle, NOT interleaved with effect execution. A stalled or slow host
  call on the driver can no longer age the lease past the 60s steal threshold, which is what
  previously let a rival session duplicate-spawn (5.6). The heartbeat thread only rewrites
  `lease.json` (atomic) and reads the clock; it makes no blocking broker calls itself.

`Koma::call` errors come back as `{ "error": "..." }` values, never `Err`
(recon gotcha) — the driver string-matches: `grant denied:` prefix = fatal misconfig
(surface in panel, stop dispatch), `koma call: timed out` / transport = retry with
backoff, verb-specific errors handled per call site below.

### 5.2 Dispatch loop (deterministic)

Capacity is a **per-bound-session** budget, not per-project (see 5.2.3): the driver owns one
`SessionCapacity` token per session it leases, counting every in-flight office spawn across
ALL projects bound to that session, and threads the remaining budget into each `kernel::step`
call. The kernel therefore never decides concurrency from a single project's view. On every
`Tick` and after every state-changing input, for each project this instance leases and whose
phase is `Running`:

1. **Reconcile** (cheap, rate-limited to every 30s): for each `OnProgress`/`Review` task
   with a binding, `agents.status { agentId }` (broker.rs:677-705). This verb returns
   `{ agentId, agent, status, liveTextLen }` while the agent is known, or a flat ERROR
   value when it is not. The killed path is driven off those error values, NOT off a status
   field: `{ "error": "unknown agentId: N" }` (koma restarted, ExtAgentRegistry cleared at
   lifecycle/mod.rs:501) and `{ "error": "session closed" }` (the bound session ended) both
   mean the agent is gone -> back to Todo (worker) / reviewer respawn (reviewer). `status:
   "gone"` is NOT emitted by `agents.status` — it is exclusively an `agents.list` reply value
   (broker.rs:643-668) and is only consumed by the orphan sweep in 9.1 step 4. A live terminal
   `status` (`done`/`error`/`killed`) is handled on the normal completion path (5.3); `queued`/
   `running` keep polling. This reconcile is the backstop for missed `agents.done` — events
   are fire-and-forget with NO replay (events.rs, EXTENSIONS.md:544-546). Because the driver's
   generic error handler (5.1) treats non-`grant denied:`/non-timeout strings as verb-specific,
   the reconcile call site matches these two exact error shapes explicitly so `session closed`
   is never mistaken for a retryable transient.
2. **Ready set** (`graph.rs`): tasks in `Todo` whose `blocked_by` are all `Done`,
   sorted by `(priority desc, id asc)` — fully deterministic.
3. **Capacity** (session-global): the office may hold at most `min(4, ...)` sub-agents in
   flight across the ENTIRE session — the union of running workers + reviewers over EVERY
   project bound to this session must stay `< 4`. The driver's `SessionCapacity` token carries
   the current session-wide running count; the kernel may emit a Spawn only while that count is
   below 4, and reviewers get slot priority over new workers so the line drains. Host cap is 5
   concurrent per SESSION with a queue (broker.rs MAX_SUBAGENTS is per-session, shared across
   every project bound to the session AND the user's own delegations); self-capping the whole
   office at 4 keeps one slot ALWAYS reserved for the user at the session level — an invariant
   that holds no matter how many projects are Running at once, because K Running projects share
   the ONE budget (drained round-robin by `(priority desc, id asc)` across projects), they do
   NOT each get 4. `config.max_workers` (clamp 1..=4) is a per-project soft sub-ceiling layered
   under the session cap: it bounds how many of the 4 shared slots any single project may hold,
   not a private allowance. A `status: "queued"` spawn reply is accepted and tracked
   identically (the id is already minted, broker.rs:564-635) and still consumes a session slot.
4. **Spawn**: `sessions.spawn_into { session: bound, task: <worker prompt>, agent:
   "office-worker", model: config.worker_model (omitted when None), notify: true }`.
   Local reply `{ agentId, status }` -> record `AgentBinding` (with `spawned_at_ms`), decrement
   the session-capacity token, persist BEFORE moving on (crash between spawn and persist is
   healed by reconcile: an unknown running office-worker in `agents.list` that no task
   references is killed as an orphan). Reply `{ status: "sent" }` means the bound session moved
   to another daemon — release the lease, stop dispatching (Limitations 13.9); the driver
   probes `sessions.list` for the bound session's liveness BEFORE firing the first spawn of a
   ready set and short-circuits the whole set if the first `spawn_into` would be cross-process,
   so a stale-lease steal can never fire a burst of untracked duplicate workers (see 4.4, 5.6).
   Errors ("session not live", spawn failures) -> task `Parked(SpawnFailed)`, notice queued.

#### 5.2.4 Per-worker runtime ceiling (the runaway backstop)

Host-contributed sub-agents run with `steps == None` because the manifest `SubAgentDef` exposes
no `steps` field (protocol.rs:61-70) and `agents.spawn` params carry no steps override, so
`merge_extension_sub_agents` builds them via `AgentDef::default()` (registry.rs:282-295) whose
`steps` is `None` (def.rs:162) -> `run_agent_loop` runs UNBOUNDED (engine.rs:229-230). Built-in
koma agents cap at 80/25 steps (registry.rs:58,68) but the office has NO host API to bound worker
iterations. A worker stuck in a tool loop keeps changing `liveTextLen`, so a liveness-based stall
detector NEVER trips, and while the extension is down (Limitation 8) nothing kills it. A hard
wall-clock ceiling is therefore the ONLY available brake on a worker's token burn.

The kernel enforces it deterministically: on every reconcile pass, any binding whose
`now_ms - spawned_at_ms > config.worker_max_runtime_ms` (default 20 min) emits a `Kill { agentId }`
effect and the task is treated as an error (worker -> Todo, attempt++; a binding that keeps
hitting the ceiling counts toward the SpawnFailed escalation). This is independent of, and
stricter than, the liveness stall detector (5.5 / failure matrix): the stall detector is a soft
"is anyone home" signal that only nudges; the runtime ceiling is an unconditional cap that fires
on BOTH a silent stall AND a runaway-but-active worker. It does not trust the model to terminate.
The ceiling is per-spawn (reset on every re-dispatch), surfaced in the panel, and configurable per
project; there is no way to disable it below a safety floor.

### 5.3 Completion, review pipeline, bounce/park/halt

On `agents.done { agentId, status }` (delivered to us as the notify:true spawner,
independent of subscriptions — events.rs:88-102) or on reconcile discovering a terminal
status:

- `FetchResult` -> `agents.result { agentId }` (broker.rs:710-741 — polling is the ONLY
  way to get report text; verified safe immediately after the event, since
  `emit_subagent_terminal` runs after results are delivered and persisted,
  subagents.rs:432-480).
- **Worker done**: parse `OFFICE-REPORT` trailer (tolerant parser, report.rs).
  `status: complete` -> task to `Review`, spawn reviewer (same spawn path, agent
  `office-reviewer`, reviewer prompt). `status: blocked` -> `Parked(WorkerBlocked)`.
  Unparseable report -> treat as complete-with-warning; the reviewer judges (never let a
  formatting miss stall the line). Comment ACKs in the trailer flip receipts to `Read`.
- **Worker error/killed**: back to `Todo`, attempt++. Three consecutive spawn-side
  failures (error before any report) -> `Parked(SpawnFailed)`.
  A comment is flipped to `Read` ONLY by an ack token; task completion never flips a receipt.
  Any comment still `Pending` when its task reaches `Done` (added mid-run, worker passed on the
  first try, never re-spawned) stays `Pending` and is surfaced in the panel as
  "unread / never delivered" (5.3 reviewer-done, task-detail 10.3) — the office never claims the
  agent read something it demonstrably never received.
- **Reviewer done**: parse `OFFICE-REVIEW` trailer. `verdict: pass` -> `Done`.
  `verdict: fail` -> bounces++, review notes stored and injected into the next worker
  prompt; if `bounces > bounce_budget` -> ESCALATION:
  1. Queue a chat.prompt nudge to the main model: task id, title, failure summary, "the
     office parked this task; advise or edit the board" (see 6.5 for budget handling).
  2. `Parked(ReviewBounceBudget)`.
- **Halt detection** (`graph.rs::line_is_stuck`): after any park, if the project has
  unfinished tasks, zero running agents, zero ready tasks, and every unfinished task is
  transitively blocked by a Parked task -> `Halted`. Queue notice "production line
  halted: <task> blocks everything", push panel alert. Resume requires human action
  (un-park / edit DAG) — the ultra-automatic grind never bypasses a human park.

### 5.4 Model bindings (user rebind, model-agnostic)

Panel sidebar lets the user set `worker_model` / `reviewer_model` per project (free-text
slug from whatever they have configured, or blank = inherit). The kernel passes the slug
in the spawn `model` param; empty string is treated as absent by the host
(broker.rs:589-596). Unresolvable slugs fall through to Main with a host toast
(EXTENSIONS.md:324-353) — we surface the same fact in the panel by never pretending to
know the catalogue. No literal "inherit" keyword exists; omission IS inherit (verified).

### 5.5 Interrupt / resume semantics

`Interrupt (hard)` — the default board button:

- Phase -> `Interrupted`; dispatch stops immediately.
- Every tracked running binding gets `agents.kill { agentId }` (broker.rs:764-834).
  Idempotent semantics matter: `{ killed: true }` means "found and now terminal" — an
  agent that finished before the kill keeps `done`, and our next reconcile will process
  its report normally, so no work is lost to the race.
- Workers `OnProgress -> Todo` (attempt preserved, NOT counted as a bounce); reviewers
  `Review { binding: None }` (reviewer respawned on resume). Desks are retained.

`Interrupt (soft drain)` — secondary option: stop dispatching new work, let in-flight
agents finish and their results be processed, then phase -> `Interrupted`.

Justification for hard-kill as default: the button is the emergency brake on a machine
whose defining feature is unbounded token burn — a brake that keeps burning until the
current 5 agents finish is not a brake. The board is durable and tasks are
idempotent-by-design (desk + delivery + report protocol), so re-dispatch on resume is
safe; the only loss is the partial in-flight attempt, which is exactly what the user
asked to stop paying for. Soft drain exists for the "wrap up cleanly" case.

`Resume`: phase -> `Running`, next tick re-dispatches. Nothing else needed — that is the
entire point of the durable board.

### 5.6 Lease-steal safety (no duplicate-spawn across sessions)

Two hazards could make a rival session daemon's office instance dispatch for a project whose
real owner is merely slow, producing two workers per task in the same desk (duplicate token
burn + concurrent writes to the same delivery paths). Both are closed:

1. **The heartbeat cannot go stale from a slow host call** — it runs on its own thread
   (5.1), so a 120s `Koma::call` on the driver can never age `lease.json` past the 60s steal
   window. The lease only goes stale if the owning PROCESS is actually dead or wedged.
2. **A non-owning session never blind-steals a live project.** Before acting on a stale lease,
   the would-be thief calls `sessions.list` and checks whether the project's `bound_session`
   is still a live session it owns locally. If `bound_session` is not one of THIS daemon's
   sessions, every `spawn_into { session: bound }` would be cross-process fire-and-forget
   (`{ status: "sent" }`, untracked, broker.rs:1809-1871) — so the driver probes the first
   spawn and short-circuits the ENTIRE ready set on `sent`, releasing the lease instead of
   firing a burst of untracked duplicate workers. Stealing only proceeds to real dispatch when
   the bound session is genuinely local to the stealing daemon (the koma-restart rebind case).

---

## 6. FRONT OFFICE (persona + channels)

### 6.1 Trio wiring

```
user <-> main chat <-> front office (persona)          user <-> panel <-> front office
```

Channels, in order of reliability:

| Direction | Channel | Notes |
|---|---|---|
| main chat -> office | contributed tools `mcp__aula-workflow__workflow_*` | TUI/standalone ONLY — see 13.1 (verified daemon-mode gap) |
| main chat -> office | workspace file inbox (6.4) | daemon-mode fallback, documented in the context blob |
| office -> main chat | `chat.prompt` | budget 10 / queue 5 / 16KB / idle-injected (broker.rs:879-912) |
| ambient awareness | `context.set` <= 8KB volatile tail (broker.rs:1590-1601) | board digest + protocol instructions |
| user <-> office | panel Office Chat view | primary surface, always works |

### 6.2 Persona over `models.invoke` (multi-turn on a single-shot API)

`models.invoke` is prompt+system only, no messages array, 32KB prompt, 25s internal
budget (broker.rs:934-1038). Multi-turn is reconstructed by the extension:

- `Project.office_transcript` holds `ChatMsg { who: User|Office, text }`.
- Each call builds: `system` = office persona (fixed text: senior delivery manager,
  negotiates scope, writes PRDs, no code) + board digest (compact). `prompt` =
  `office_summary` (rolling) + last N transcript turns + the new user message +
  an output-contract instruction.
- Folding: when the assembled prompt exceeds 24KB, the oldest half of the transcript is
  summarized INTO `office_summary` by one extra `models.invoke` call ("summarize this
  requirements conversation, keep decisions and open questions"), then dropped from the
  transcript. Caps respected by construction; `prompt exceeds 32KB` is treated as a bug
  and additionally guarded by a hard truncate.
- Every `models.invoke` here — persona reply, the 2-4 PRD-authoring calls, the breakdown call,
  and the fold summarize call — is emitted by the kernel as an `InvokeModel` effect and executed
  on the invoke worker pool (5.1), NEVER inline on the driver tick loop. A PRD flow is 25-125s of
  blocking that would otherwise freeze dispatch, reconcile, the heartbeat, and the 100ms
  panel-read oneshots; running it off the driver is what keeps "NO LLM in the control loop"
  literally true. Results return as `Command::InvokeResult` on `CMD_TX`.
- Timeouts (`model call timed out`) -> one retry (re-emitted as a fresh `InvokeModel`), then
  surface "office did not answer; try again" to the caller. Role = `config.office_role`, default
  "main"; "unknown role" is surfaced verbatim (host never silently falls back — broker.rs:934-1038).

### 6.3 PRD -> breakdown -> authorization flow

1. **Drafting**: user talks to the office (panel chat / workflow_brief / inbox). Office
   asks questions, negotiates scope. On "write the PRD", the extension drives 2-4
   invokes (outline, then sections — each under the 25s budget) and stores `prd_markdown`.
2. **Breakdown**: office is asked (one invoke, JSON output contract) for epics/stories/
   tasks with `blocked_by` edges, acceptance criteria, priorities. The kernel VALIDATES:
   parse (one re-ask on failure with the parse error quoted), slug uniqueness, DAG
   acyclicity (graph.rs), non-empty acceptance. The validated breakdown lands on the
   board in `Backlog`/`Todo`; the user can edit everything in the panel BEFORE go.
   The LLM proposes; the deterministic kernel only ever accepts validated structures.
3. **Authorization** (the hard gate): `workflow_authorize` / panel button requires
   `delivery_path`. Validation: absolute; extension `mkdir -p`s it (we are a normal
   process); must be inside the bound session workspace (7.1) unless the user sets the
   documented `allow_outside_workspace` escape hatch (workers then must use bash for
   delivery writes — Limitations 13.5). No delivery path = the project CANNOT leave
   `Ready`. Phase -> `Running`, the grind starts.

### 6.4 Daemon-mode bridge: the file inbox

Because contributed tools are invisible to the model in `--daemon` sessions (verified:
lifecycle/mod.rs:234-240 captures `mcp_manager = None` before extension start and never
re-registers; mcp/mod.rs:683-693 Proxy no-op), the main chat still needs a write path to
the office in the GUI. The context blob instructs the model:

> To reach Workflow, write a JSON file into `<workspace>/koma-workflow/inbox/`
> named `<millis>-<slug>.json`: `{ "op": "brief"|"status"|"authorize"|"interrupt"|
> "resume"|"comment", ... }`. The office replies in chat.

The driver polls the inbox each tick (1s), consumes files (move to
`koma-workflow/inbox/processed/`), feeds them to the kernel as `Command`s, and answers via
`chat.prompt` (budget-aware) and the panel. Workspace file writes are exactly what the
model's file tools are allowed to do, so this works identically in TUI and GUI. It is
ugly and it is documented as the host-API workaround it is.

### 6.5 `chat.prompt` discipline (office speaks up)

Notices (`project done`, `task parked`, `line halted`, `office needs a human`) go through
a durable per-project `outbox`:

- Send at most one buffered notice per tick; text <= 4KB (well under the 16KB cap).
- Reply `{ queued: n }` -> mark sent (delivery still waits for session idle — the host
  buffers and injects as one synthetic user turn, broker.rs:879-912; we never assume
  immediacy).
- Error `prompt queue full (5)` -> keep in outbox, retry next tick.
- Error `extension turn budget exhausted; waiting for user activity` -> mark the outbox
  PAUSED; retry only after the next `agent.turn_end` event (a user turn resets the
  budget; the error string is our only signal — recon-verified, no push notification).
- Consecutive-dup dedupe host-side is harmless to us (identical repeat = still queued).
- Every notice is ALSO always visible in the panel, which is the authoritative surface;
  chat.prompt is best-effort by design.

### 6.6 `context.set` blob (<= 8KB)

Built by `digest.rs`, republished on start (memory-only host-side — verified rest.rs:461)
and on every material change, byte-length-guarded at 7900 bytes (host cap is 8192 BYTES,
boundary-inclusive, broker.rs:2784):

```
# Workflow
Active projects: <n>. Panel: Workflow tab.
- <project>: phase=<...> done <d>/<t> running=<r> parked=<p> delivery=<path>
  attention: <halt/park one-liners, max 2>
To reach the office from chat, write koma-workflow/inbox/<millis>-<slug>.json:
{"op":"brief","project":"<id>","message":"..."} (ops: brief,status,authorize,interrupt,resume,comment)
```

Projects are listed most-recently-active first and truncated to fit; the instruction
block is never truncated.

---

## 7. Desks and delivery

### 7.1 Layout (inside the bound session workspace — containment-driven)

```
<workspace>/koma-workflow/
  DO-NOT-ENTER.md                  # "Workflow working area. Agents operate here.
                                   #  Humans: read-only please. Managed by aula.workflow."
  .gitignore                       # contains "*" so the whole area never enters the user's VCS
  inbox/                           # 6.4
  desks/
    <project-slug>/
      <task-slug>--koma-workflow-desk/   # per-task desk, human-readable, obviously marked
```

- The EXTENSION creates and removes desk dirs directly with `std::fs` (`EnsureDesk`
  effect). Decision and justification: the extension is a normal OS process, so
  deterministic directory management belongs to deterministic code — relying on agent
  obedience to `mkdir` its own isolation would make isolation probabilistic. Workers are
  INSTRUCTED (absolute paths in the prompt) to keep all scratch inside their desk and
  final artifacts inside the delivery path.
- No git worktrees in v1: worktrees would put office branches inside the user's repo and
  require the extension to run `git` against it. Desks are plain directories; the
  `.gitignore` guarantees zero VCS pollution; workers that need repo context read the
  workspace read-only and write only desk + delivery. (Documented trade-off: two tasks
  editing the same delivery file serialize via `blocked_by` edges — the breakdown
  contract instructs the office to add edges between tasks sharing delivery files.)
- Cleanup policy: desk retained through bounces/parks (the next attempt reuses it and the
  prompt says so); deleted when the task reaches `Done` (configurable `keep_desks`
  flag for debugging); the whole `desks/<project>` tree deleted when the project is
  archived from the panel. Interrupt never deletes desks.

### 7.2 Why not `/tmp` or `~`: file-tool containment

`resolve()` (tool/mod.rs:283-330) confines write/edit tools to the session workdir
allow-list plus `/tmp/koma` scratch. `/tmp/koma` is not durable and not human-visible;
anywhere outside the workspace breaks worker file tools (bash could still write there,
but building the line on the bash bypass would be fragile and harness-hostile). Hence:
desks and delivery inside the workspace, enforced at authorization.

---

## 8. Worker and reviewer prompt engineering (`prompts.rs`)

### 8.1 Worker spawn prompt (assembled per attempt; target < 12KB)

```
You are a Workflow worker on one task of a larger production line. Work autonomously;
no human will answer questions mid-task.

PROJECT: <name> — <one-line intent>
EPIC: <title> — <intent>        STORY: <title> — <intent>
TASK <task-id>: <title>
<description>

ACCEPTANCE CRITERIA (the reviewer will check exactly these):
- <criterion> ...

WORKSPACE RULES
- Your desk (all scratch, notes, intermediate files): <abs desk path>
- Deliverables go ONLY to: <abs delivery path>[/<task-specific subpath if set>]
- Do not touch anything else in the repository. Do not commit, push, or modify VCS state.
- You cannot change directories; use absolute paths.

PRIOR ATTEMPTS (present only when attempt > 1 or bounced):
- Attempt <n> review notes: <reviewer failure reasons, truncated>

COMMENTS FROM THE BOARD (ack every id you read):
- [c17] <text> ...

REPORT PROTOCOL — end your final message with exactly this block:
OFFICE-REPORT
status: complete | blocked
summary: <what you did, 3-6 lines>
delivered: <newline-separated absolute paths you created/updated under the delivery path>
ack-comments: c17,c18
blocked-reason: <only when status: blocked — what a human must decide>
```

The `agent` field is `office-worker`, whose manifest `prompt` (system) carries the
persona and the hard rules restated; the spawn `task` carries the per-task material.
Sub-agent completion never auto-wakes the chat model (broker.rs:564-635 — non-detached,
no tool_call_id), so the office fully owns the loop.

### 8.2 Reviewer spawn prompt

```
You are a Workflow reviewer. Judge ONE task against its acceptance criteria. You did
not write this work. Be strict; a false pass ships broken work.

TASK <task-id>: <title> / criteria list
WORKER SUMMARY: <worker report summary + delivered paths>
CHECK: read every delivered file under <delivery path>; verify each criterion; run
read-only checks where possible (build/typecheck allowed; nothing destructive; nothing
outside the delivery path and desk).

VERDICT PROTOCOL — end with exactly:
OFFICE-REVIEW
verdict: pass | fail
reasons: <numbered, tied to criteria; required on fail>
```

### 8.3 Report parsing (`report.rs`)

Tolerant scanner: find the LAST `OFFICE-REPORT` / `OFFICE-REVIEW` marker, parse
`key: value` lines until blank/EOF, unknown keys ignored, missing block handled per 5.3.
Fully unit-tested against sloppy model output (markdown fences, prose after the block,
uppercase drift).

---

## 9. Failure matrix

| Failure | Detection | Recovery |
|---|---|---|
| Extension crashes mid-project | koma does NOT restart it (EXTENSIONS.md:790-792); in-flight sub-agents keep running in the daemon | Next start trigger (koma boot, or first panel.msg when the user opens the tab): load state, re-lease, reconcile 9.1. Missed `agents.done` events are permanently lost (no replay — verified) but reconcile polls `agents.status`/`agents.result` for every bound task, so nothing is stranded. Documented: with the panel closed and no koma restart, the office stays down until the user opens the tab (13.8). |
| koma daemon restarts | all sub-agents dead, ExtAgentRegistry cleared, ext_context + chat buffers wiped (memory-only, verified) | Extension auto-starts at boot (lifecycle/mod.rs:241). Reconcile: `agents.status` on stale bindings returns `unknown agentId` -> workers to Todo, reviewers respawn. Republish context blob; outbox re-sends notices. Board state = disk = truth. |
| Worker dies silently / event missed | 30s reconcile poll (5.2.1) | terminal `status` handled identically to the event path; the `agents.status` ERROR values `unknown agentId: N` (registry cleared) and `session closed` (session ended) are the killed path — `status:"gone"` is NOT an `agents.status` value (it is `agents.list`-only, used by the orphan sweep 9.1.4). |
| Worker runs too long (runaway OR silent stall) | HARD ceiling: reconcile force-kills any binding older than `config.worker_max_runtime_ms` (default 20 min), regardless of liveness (5.2.4) | `Kill` effect + task -> Todo (attempt++); repeated ceiling hits count toward SpawnFailed escalation. This is the ONLY bound on a worker's token burn — the host cannot cap ext sub-agent steps (SubAgentDef has no `steps` field; `agents.spawn` has no steps param), so a wall-clock kill is the sole backstop. |
| Worker silent-but-slow (soft signal) | `liveTextLen` from `agents.status` unchanged across `worker_stall_ticks` (default 15 min) | soft: notice to panel + escalation nudge so the user can hard-kill early; the 20-min runtime ceiling above still fires unconditionally even for a legitimately long build. |
| Spawn races the 5-slot cap | host returns `status: "queued"` with a real id | tracked normally; kernel counts it against our self-cap so we never flood the queue. |
| User edits board mid-flight | all mutations arrive as `Command`s through the same kernel queue | applied between transactions; moving a Running card asks "kill the worker?" in the panel; DAG edits re-validate acyclicity before commit; a Running task cannot be deleted, only interrupted. |
| Two koma sessions racing | per-project lease + flock (4.4) | non-holders are read-only (+comments); stale lease (60s) is stolen; `spawn_into` returning `"sent"` = binding moved, lease released. |
| Panel push dropped (256-entry outbox drop-oldest, closed tab) | pushes are full snapshots with `seq` | panel detects `seq` gap irrelevance (snapshot replaces state); on load/reload it rehydrates via request/reply. Deltas are never trusted for state (10.3). |
| Office LLM timeout / garbage JSON | 25s budget error string; parse failure | one retry / one re-ask with the error quoted; then surface to user. The kernel never blocks on the LLM. |
| chat.prompt budget exhausted | error string (only signal, verified) | outbox PAUSED until next `agent.turn_end`; panel remains authoritative. |
| Delivery path deleted mid-run | spawn-time `mkdir -p` + per-dispatch existence check | dispatch pauses with panel alert "delivery path missing", phase untouched. |

### 9.1 Start-up reconciliation (exact order)

1. Load registry + all projects; validate schema version.
2. For each project: acquire or observe lease.
3. For leased `Running`/`Interrupted` projects: for every task binding, `agents.status`;
   live terminal `status` -> `agents.result` -> normal completion path; the error values
   `unknown agentId: N` / `session closed` -> killed path; live `running`/`queued` past the
   runtime ceiling (5.2.4) -> force-kill + re-queue; otherwise -> keep, resume polling.
4. `agents.list` sweep: any `office-worker`/`office-reviewer` owned by us (including entries
   whose `agents.list` status is `gone`) that no task references -> `agents.kill` (orphan from a
   crash between spawn and persist). `status:"gone"` is consumed HERE, from `agents.list`, not
   from `agents.status`.
5. Republish context blob; push panel snapshot; resume dispatch on next tick.

---

## 10. PANEL (`ui/`)

### 10.1 Build

Vite + React 19 + TypeScript + Tailwind 4 + framer-motion for board/graph animation,
inline SVG for the dependency mini-map and burn-down sparklines. `vite.config.ts` sets
`base: './'` — asset URLs must be relative because the app is served at
`koma://extension/aula.workflow/index.html` and absolute `/assets/...` would drop
the ext-id path segment and 404 (gui/mod.rs:106-133; hashed `assets/*.js|*.css` get
correct MIME per mod.rs:45-59 — no `.ico`/`.map` reliance, favicon is an inline SVG data
URI). `koma-panel.js` is copied verbatim and loaded from `index.html`; the app uses
`KomaPanel.send` (default 15s timeout — kept ABOVE the host's 10s panel.msg invoke so
real host errors surface, per recon) and `KomaPanel.onPush`.

### 10.2 Message protocol (panel <-> daemon)

Panel -> daemon (`panel.msg` arrives at `on_invoke` with `{ panelId, payload }`,
10s host timeout — handlers must answer fast and never `Koma::call`):

```
{ op: "hello", uiVersion }                 -> reply { ok, snapshot }        // rehydrate
{ op: "state", project? }                  -> reply { ok, snapshot }
{ op: "office_chat", project, message }    -> reply { ok, accepted: true }  // async; answer arrives as push
{ op: "authorize", project, deliveryPath, allowOutsideWorkspace? }
{ op: "interrupt", project, mode }         // "hard" | "soft"
{ op: "resume", project }
{ op: "card_move", task, to, killWorker? } // column move; guarded transitions
{ op: "comment_add", task, text }
{ op: "unpark", task }  { op: "edit_task", ... }  { op: "edit_deps", ... }
{ op: "config_set", project, maxWorkers?, bounceBudget?, workerModel?, reviewerModel? }
{ op: "project_create", name }  { op: "project_archive", project }
{ op: "prd_get", project }                 -> reply { ok, prd }
```

Commands are pushed to the kernel channel; the handler replies `{ ok: true, accepted }`
immediately (deadlock rule) — results arrive as pushes. Rehydrate (`hello`/`state`)
replies carry the snapshot INLINE in the invoke result: the reply direction has no
256KiB cap (that cap is panel->host only, panelBridge.ts:133-149) and rides the 4MiB
wire frame; the snapshot builder hard-fails a project into "summary mode" (reports
truncated to 4KB) long before 3MB.

Daemon -> panel: `panel_push("board", envelope)` where envelope is ALWAYS
`{ kind: "snapshot", seq, projects: [...] }` — full-state snapshots, never deltas.
Rationale (recon-verified): pushes are lossy at two layers (256-entry drop-oldest
daemon outbox, rest.rs:424-434; fire-and-forget GUI postToPanel) and panel_push
silently drops >1MiB payloads (sdk.rs:220-239). Snapshots make loss harmless.
Size guard: serialized snapshot > 900KB -> per-project summary mode (drop reports/
history bodies, keep counts and states) and set `truncated: true`; the panel fetches
task detail on demand via `{ op: "task_detail", task }`. Pushes are throttled to one
per 250ms, coalescing dirty flags.

### 10.3 Views

- **Dashboard** (default): multi-project cards — phase badge, done/total ring, running
  workers, parked count, token-burn proxy (spawn count), last notice; framer-motion
  layout animation; a global "ALL LINES" halt indicator.
- **Board**: five columns (backlog/todo/onprogress/review/done); Parked cards render in
  Review with an amber "parked" badge; drag between legal columns only (kernel guards
  anyway); running cards show agent id + liveTextLen-based activity pulse; blocked-by
  chips on each card; project Interrupt (hard) / Drain (soft) / Resume buttons with
  confirm.
- **Drilldown**: project -> epic -> story -> task tree with progress rollups and an SVG
  dependency mini-map (topo-sorted lanes; parked nodes highlighted; the halt-culprit path
  drawn in red).
- **Task detail**: description, acceptance checklist, state history, attempts/bounces,
  worker report + review verdict (monospace blocks), comments thread with receipt states
  (pending / delivered / read, with timestamps); a comment still `pending` on a `Done` task is
  flagged "never delivered — reopen to send" so the user is never misled into thinking the agent
  read it (5.3). Un-park, kill-worker, priority editor.
- **PRD viewer**: rendered markdown of `prd.md`, read-only, with the office-chat pane
  beside it during Drafting.
- **Office chat**: the panel's direct line to the persona (6.2); shows the transcript,
  folding indicator, and the outbox/notice log with sent/paused states.
- **Settings sidebar**: per-project model bindings (free-text slugs, blank = inherit
  Main), max workers, bounce budget, keep-desks toggle, state-root path display.

Extension detail page: the host's InstalledExtensionTab only shows metadata + panel count
(InstalledExtensionTab.tsx:81, recon); we do not modify it. "Status on the detail page"
is satisfied by what the host already renders; live status lives in our panel
(Limitations 13.10).

---

## 11. Contributed tools (TUI surface) — exact contracts

All handled in `on_invoke(method = "tool.call", params = { name, args })`
(mcp/mod.rs:610-617, verified); the handler queues a `Command` and, for the synchronous
ones, waits on a bounded oneshot from the kernel thread (budget 100ms for reads, and for
`workflow_brief` the kernel replies "office is thinking; answer will arrive via chat" while
the invoke returns — the real reply is delivered by `chat.prompt`, keeping handlers far
under the 120s invoke ceiling and off the serve loop). Replies use the `{ "output":
string }` convention (host extracts `output`, mcp/mod.rs:613-617). Advertised to the
model as `mcp__<sanitized aula.workflow>__workflow_*`.

| Tool | args | output |
|---|---|---|
| workflow_brief | { message, project? (default: single Drafting project) } | ack; persona reply arrives via chat.prompt |
| workflow_status | { project? } | compact board digest text (same builder as context blob, uncapped section) |
| workflow_authorize | { project, delivery_path } | "authorized, line running" or the exact validation error |
| workflow_interrupt | { project, mode? } | confirmation + what was killed/drained |
| workflow_resume | { project } | confirmation |
| workflow_comment | { task, text } | "comment c<id> filed (receipt: pending)" |
| workflow_projects | {} | one line per project: id, phase, progress |

---

## 12. Packaging, dev loop

- `pack.sh`: `cargo build --release -p office-daemon` + `cd ui && npm run build` ->
  stage `manifest.json` (runtime.exec rewritten to `bin/office-daemon`), `bin/office-daemon`,
  `ui/` (Vite `dist/`) -> `dist/workflow.zip` (mirrors src-extension/pack.sh layout,
  README.md:170-179).
- Dev install (`dev-install.sh`): there is NO local-install verb (verified — install only
  flows through the koma.run store, requests_ext.rs:43, with an unsigned fallback only in
  debug builds). The script therefore: unpacks the zip into
  `~/.koma/extensions/aula.workflow/`, upserts the `installed_extensions` entry
  `{ id, version, tier: "free", granted: [<requires wire strings>], enabled: true,
  kind: "daemon", exec: "bin/office-daemon" }` into `~/.koma/config.json` (jq), and tells
  the user to restart koma. Boot auto-start does the rest (lifecycle/mod.rs:241). This is
  the recon-documented "works by construction" path; it never touches the simple-coders
  tree. Standalone protocol testing uses the SDK demo mode (`cargo run -p office-daemon`
  with no `KOMA_EXT_SOCKET`).

---

## 13. LIMITATIONS (host API, no workaround possible without host changes)

1. **Contributed tools are invisible in `--daemon` sessions** (GUI / daemon-per-session):
   `mcp_manager` is `None` when extensions register at boot in daemon mode and tools are
   never re-registered (lifecycle/mod.rs:186-199 + 234-240); the Proxy backend is a
   silent no-op (mcp/mod.rs:683-693). `workflow_*` tools work only in TUI standalone.
   Mitigation shipped: panel + file inbox (6.4) + context.set instructions.
2. **No API to message a RUNNING sub-agent.** Comments added mid-run reach the agent only
   at the next spawn boundary (bounce, review, resume) — receipts make this visible. An
   "urgent" comment offers kill+respawn.
3. **Events have no replay** (events.rs; EXTENSIONS.md:544-546). Mitigated by the 30s
   reconcile poll; a fully closed loop is impossible if the extension is down AND the
   daemon dies before restart — reconcile after restart covers state, not lost reports of
   sessions that no longer exist.
4. **chat.prompt is best-effort**: idle-injected, queue 5, 16KB, 10-turn budget with the
   error string as the only signal (broker.rs:879-912). The office can be muted for
   arbitrarily long if the user never types. Panel is the authoritative notice surface.
5. **Worker file tools are confined to the session workspace** (tool/mod.rs:283). Delivery
   path and desks must live inside it; `allow_outside_workspace` exists but relies on
   bash writes (subject to harness approval settings).
6. **models.invoke is single-shot, 32KB, 25s** — long PRDs are built in multiple calls
   and conversations are folded; a model that cannot answer in 25s cannot be the office.
7. **No local install verb**: dev loop requires the manual registry edit (12) or a debug
   build against the store's unsigned fallback.
8. **No auto-restart after extension crash** (EXTENSIONS.md:790-792): with the GUI panel
   closed, the office stays down until koma restarts or the user opens the tab / an
   oauth/panel trigger fires. The line resumes cleanly but does not tick while down.
9. **Cross-daemon spawns are untrackable** (`{ status: "sent" }`, no agentId,
   broker.rs:1809-1871): one office instance per session daemon owns its projects; a
   project whose session moves daemons pauses until the new daemon's instance leases it.
10. **Extension detail page is host-owned**: it shows only manifest metadata and panel
    count; no live status can be injected there.
11. **Only `panels[0]` is launchable** (ActivityBar.tsx:78-91): single panel, internal
    routing.
12. **Panel asset serving**: no caching headers, limited MIME map, no CSP; fetch/network
    behavior from panel pages is unverified on WebKitGTK — the UI therefore talks ONLY
    through the bridge.
13. **`sessions.switch` validates nothing cross-daemon** (bogus uuid -> `signaled`,
    broker.rs:1691-1720) — we never rely on it; the office only reads `sessions.list`.
14. **Sub-agent slots are shared with the user** (MAX_SUBAGENTS=5 per SESSION, pooled across
    every project bound to that session AND the user's own delegations): the office self-caps
    the WHOLE session at 4 via a per-bound-session capacity token (5.2.3), not per-project, so
    N concurrently Running projects still cannot exceed 4 combined and one slot stays reserved
    for the user. A user burst can still queue office spawns (host queue keeps ids valid, so
    tracking survives).
