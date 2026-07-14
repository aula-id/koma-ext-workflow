# Workflow — Build Waves

Ordered implementation plan for the design in `ARCHITECTURE.md`. Every wave is
**independently green**: after it lands, `cargo build --workspace` (and the panel's
`npm run build` for UI waves) succeed and all `*_test.rs` pass. Rust waves and panel
waves are separable; the only cross-boundary contract is the panel message protocol
(ARCHITECTURE.md 10.2), frozen in W7 and consumed by the UI waves.

Conventions:
- Tests live in `*_test.rs` beside the module (house rule). Pure crates use plain unit
  tests; store/daemon use `tempfile` + a scripted `FakeHost`.
- No LLM in the control loop — every kernel wave is tested with deterministic inputs.
- Commit at every green checkpoint (one commit per wave unless noted). No emoji, no
  co-author line.
- Host is READ-ONLY at `/media/wangsa/project-x/simple-coders`; the SDK is a path dep.

Dependency graph of waves:

```
W1 ─ W2 ─ W3 ─ W4 ──┐
                    ├─ W7 ─ W8 ─ W9 ─ W10(pack)
W5 ─────────────────┤
W6 ─────────────────┘
WU1 ─ WU2 ─ WU3 ─ WU4      (needs the frozen protocol from W7; otherwise parallel to Rust)
```

---

## W1 — Workspace scaffold + domain model

**Goal:** compiling workspace, the manifest, and the pure domain types. No host, no IO.

**Files touched**
- `Cargo.toml` (workspace: members `crates/office-core`, `crates/office-store`,
  `crates/office-daemon`)
- `manifest.json` (exactly as ARCHITECTURE.md 1.manifest.json)
- `crates/office-core/Cargo.toml` (deps: serde, serde_json)
- `crates/office-core/src/lib.rs` (module wiring)
- `crates/office-core/src/domain.rs`
- `crates/office-core/src/domain_test.rs`
- `.gitignore` (target/, ui/dist/, node_modules/, dist/)

**Scope**
- All structs/enums from ARCHITECTURE.md 3: `ProjectId/EpicId/StoryId/TaskId/CommentId`,
  `Column`, `TaskState`, `ParkReason`, `Task`, `Comment`, `Receipt`, `Story`, `Epic`,
  `ProjectPhase`, `Project`, `ProjectConfig`, `AgentBinding`, `AgentKind`, `ChatMsg`,
  `TaskEvent`, `OutboundNotice`. All `Serialize`/`Deserialize`, `Clone`, `Debug`.
- `Column` projection from `TaskState` (`fn column(&TaskState) -> Column`, Parked -> Review).
- Id minting helpers (slug + monotonic suffix) as pure functions.
- Serde round-trip stability: a `schema` version constant `SCHEMA_V: u32 = 1`.

**Test plan (`domain_test.rs`)**
- serde round-trip for a fully-populated `Project` (every enum variant instantiated once).
- `column(&TaskState)` maps each state to the right column; Parked -> Review.
- slug minting: collisions get monotonic suffixes; slugs are `[a-z0-9-]` only.
- Manifest parses: `serde_json::from_str::<...>()` is exercised later in W6; here just
  assert the file is valid JSON via a build-time `include_str!` + `serde_json::from_str`
  into `serde_json::Value` in a test.

**Green commit:** `feat(core): workspace scaffold + domain model`

---

## W2 — State machines + DAG graph

**Goal:** deterministic task/project transitions and blocked-by graph logic.

**Files touched**
- `crates/office-core/src/machine.rs` + `machine_test.rs`
- `crates/office-core/src/graph.rs` + `graph_test.rs`
- `crates/office-core/src/lib.rs` (export)

**Scope**
- `machine::step_task(state, TaskTransition) -> Result<TaskState, Transition>` covering
  every edge in ARCHITECTURE.md 3 task machine (Backlog->Todo, Todo->OnProgress,
  OnProgress->{Review,Todo,Parked}, Review->{Done,Todo,Parked}, Parked->Todo, hard-interrupt
  normalize). Illegal edges return `Err(Transition)`.
- `machine::step_project(phase, ProjectTransition) -> Result<ProjectPhase, Transition>` for
  the project machine (Drafting->Ready->Running<->Interrupted, Running->Halted->Running,
  Running->Done). Delivery-path gate enforced (Ready->Running requires a flag param).
- `graph::validate_acyclic(&[Task]) -> Result<(), Cycle>` (Kahn's algorithm).
- `graph::ready_set(&[Task]) -> Vec<TaskId>`: Todo tasks whose `blocked_by` are all Done,
  sorted `(priority desc, id asc)`.
- `graph::line_is_stuck(&Project) -> Option<StuckReason>`: unfinished tasks exist, zero
  running, zero ready, every unfinished task transitively blocked by a Parked task.

**Test plan**
- `machine_test.rs`: table-driven — every legal transition succeeds, a sample of illegal
  ones return `Err`; delivery-path gate blocks Ready->Running when absent.
- `graph_test.rs`: acyclic passes, injected cycle fails; ready-set ordering is
  deterministic under priority ties; `line_is_stuck` true only when all four conditions
  hold (parametrized: one running agent -> not stuck; one ready task -> not stuck).

**Green commit:** `feat(core): task/project state machines + blocked-by DAG`

---

## W3 — Prompt builders, report parser, digests

**Goal:** all pure string production/parsing: worker/reviewer prompts, OFFICE-REPORT /
OFFICE-REVIEW parsing, context blob + panel snapshot digests with size caps.

**Files touched**
- `crates/office-core/src/prompts.rs` + `prompts_test.rs`
- `crates/office-core/src/report.rs` + `report_test.rs`
- `crates/office-core/src/digest.rs` + `digest_test.rs`
- `crates/office-core/src/lib.rs` (export)

**Scope**
- `prompts::worker(project, task, desk, delivery, attempt, review_notes, comments) -> String`
  (ARCHITECTURE.md 8.1) and `prompts::reviewer(...)` (8.2). Deterministic; assert target
  size < 12 KB and hard-truncate the variable sections (review notes, description) to keep
  the whole prompt bounded.
- `prompts::office_system(digest) -> String` persona head for `models.invoke`.
- `report::parse_report(&str) -> ReportTrailer` and `parse_review(&str) -> ReviewTrailer`:
  tolerant scanner (last marker wins, `key: value` until blank/EOF, unknown keys ignored,
  case-insensitive keys, markdown-fence tolerant). Missing block -> `Unparseable`.
- `digest::context_blob(&[Project]) -> String` capped at 7900 bytes (byte length, host cap
  8192 inclusive per broker.rs:2784), instruction block never truncated, projects dropped
  tail-first.
- `digest::panel_snapshot(&[Project], mode) -> Value` (full vs summary/truncated) — the
  serialized-size guard lives in the driver (W7); here the builder just supports both modes.

**Test plan**
- `prompts_test.rs`: golden-ish assertions on required sections present; truncation keeps
  total under cap when fed oversized inputs; comment ids appear in the ack instruction.
- `report_test.rs`: sloppy inputs (prose after block, fenced block, uppercase drift,
  duplicate blocks -> last wins, missing block -> Unparseable, `status: blocked` +
  blocked-reason captured, ack-comments list parsed).
- `digest_test.rs`: blob always <= 7900 bytes even with many projects; instruction block
  survives; snapshot summary mode drops report/history bodies.

**Green commit:** `feat(core): prompt builders, report parser, size-capped digests`

---

## W4 — The kernel (pure dispatch/review/bounce/park/halt)

**Goal:** `office-core::kernel::step` — the deterministic brain. No IO; emits `Effect`s.

**Files touched**
- `crates/office-core/src/kernel.rs` + `kernel_test.rs`
- `crates/office-core/src/lib.rs` (export `Input`, `Effect`, `Command`, `HostEvent`, `step`)

**Scope** (ARCHITECTURE.md 5.1–5.3)
- `Input { Command(Command), Host(HostEvent) }`, `Effect { Spawn, Kill, FetchResult,
  InvokeModel, PublishContext, QueueChatPrompt, PanelPush, EnsureDesk, Persist }`.
- `step(&mut Project, Input, now_ms, session_capacity) -> Vec<Effect>`: capacity is passed IN
  as a session-global remaining-slot count (the driver owns it across all leased projects,
  5.2.3), NOT computed per-project.
  - dispatch: ready-set (W2) x session capacity (`< min(4, ...)` combined across projects,
    `max_workers` a per-project soft ceiling, reviewers get slot priority) -> `EnsureDesk` +
    `Spawn` effects; record binding on the returned agent id via a follow-up
    `HostEvent::Spawned { agent_id, spawned_at_ms }` (kernel stays pure — the driver feeds ids
    and clock back).
  - completion: `HostEvent::AgentsDone` -> `FetchResult`; `HostEvent::Result` -> parse
    (report.rs) -> Review + reviewer spawn / Parked / Todo re-queue with attempt++.
  - runtime ceiling (5.2.4): on reconcile input, any binding older than
    `config.worker_max_runtime_ms` -> `Kill` + Todo re-queue (attempt++); the ONLY bound on
    runaway token burn (host cannot cap ext sub-agent steps).
  - review: verdict pass -> Done; fail -> bounces++, notes stored, re-queue or (over budget)
    escalate: `QueueChatPrompt` nudge + `Parked(ReviewBounceBudget)`.
  - halt: after any park, `graph::line_is_stuck` -> project `Halted` + notice.
  - interrupt/resume: hard (Kill all bindings + normalize states) vs soft (stop dispatch,
    drain) per 5.5; resume re-arms dispatch.
  - comment receipts: Delivered at spawn (folded into prompt), Read ONLY on an ack token and
    ONLY from a prior Delivered; task completion NEVER flips a receipt — a comment still Pending
    at Done stays Pending (5.3, domain.rs Receipt).
  - every state-mutating branch emits `Persist` and a `PanelPush{snapshot}`.

**Test plan (`kernel_test.rs`)** — the heaviest test wave; the kernel is the correctness core:
- dispatch respects deps, priority order, and the SESSION-GLOBAL capacity cap: two Running
  projects sharing one session emit at most 4 combined Spawns (not 4 each) and always leave one
  slot; queued-status spawn tracked and still consumes a session slot.
- runtime ceiling: a binding whose `spawned_at_ms` is older than `worker_max_runtime_ms` on a
  reconcile input emits `Kill` + Todo re-queue regardless of any liveness signal.
- receipt discipline: a comment added mid-run is Pending; on a first-try pass to Done it stays
  Pending (never flips to Read); Read only on an ack token that had a prior Delivered.
- a full task lifecycle: Todo -> spawn -> AgentsDone(done) -> FetchResult -> Review ->
  reviewer spawn -> pass -> Done, asserting the exact Effect sequence.
- bounce path: fail up to budget re-queues with attempt++ and injects review notes; over
  budget -> QueueChatPrompt + Parked.
- worker error/killed -> Todo re-queue; three spawn-failures -> Parked(SpawnFailed).
- halt: construct a project where the only unfinished task is blocked by a parked task ->
  Halted effect + notice; adding a ready task un-stucks.
- hard vs soft interrupt effect sets; resume re-dispatches.
- comment receipt transitions (delivered on spawn, read on ack token in a fed Result).
- determinism: same inputs -> identical Effect vec (run twice, assert equal).

**Green commit:** `feat(core): deterministic dispatch/review/bounce/park/halt kernel`

---

## W5 — Persistence store + lease

**Goal:** durable, atomic, versioned store + cross-instance dispatch lease. Survives ext
restart and koma restart.

**Files touched**
- `crates/office-store/Cargo.toml` (deps: office-core, serde_json; dev: tempfile)
- `crates/office-store/src/lib.rs`
- `crates/office-store/src/store.rs` + `store_test.rs`
- `crates/office-store/src/lease.rs` + `lease_test.rs`

**Scope** (ARCHITECTURE.md 4)
- `store::root()` = `${WORKFLOW_HOME:-~/.koma-workflow}`; layout `registry.json`,
  `projects/<slug>/{state.json,prd.md,journal.ndjson,lease.json}`; write `README.md` +
  `DO-NOT-ENTER.md` on init.
- Atomic write: tmp + fsync + rename for `state.json`/`registry.json` + dir fsync after rename.
  Journal append with torn-tail tolerance on read.
- Cross-file consistency (4.2): create writes `state.json` before adding the registry row;
  archive removes the registry row before deleting the state dir. Load is self-healing — adopt
  a `state.json` with no registry row, drop/quarantine a registry row whose `state.json` is
  missing or corrupt (`projects/.quarantine/`).
- Schema: `"schema": "workflow/1"`; loader refuses newer major, runs ordered
  migration table (empty for v1).
- `lease::acquire/heartbeat/release` (ARCHITECTURE.md 4.4): `{instance,session,pid,
  heartbeat_ms}`, steal on 60s-stale, advisory `flock` around load-mutate-store; read-only
  mode when not held (comments still allowed via the flock).

**Test plan**
- `store_test.rs` (tempfile root): save+load round-trip; atomic rename leaves prior file on
  simulated mid-write (write to tmp, do not rename, assert old intact); torn journal tail
  skipped; newer schema refused; migration chain runs for an older file. Cross-file crash
  simulation: state dir with no registry row -> adopted on load; registry row with missing/
  corrupt `state.json` -> dropped + quarantined; create/archive write order asserted.
- `lease_test.rs`: acquire when free; second instance blocked; stale (60s) stolen;
  heartbeat refresh; concurrent flock'd comment add from two handles serializes without
  torn write (spawn threads).

**Green commit:** `feat(store): atomic versioned store + dispatch lease`

---

## W6 — Daemon skeleton: SDK glue, Host trait, handlers

**Goal:** the shipped binary connects to koma (or runs demo mode), routes invokes/events to
an mpsc channel, never calls `Koma::call` from a handler. No kernel wiring yet.

**Files touched**
- `crates/office-daemon/Cargo.toml` (deps: koma-extension path dep, office-core, office-store,
  serde_json)
- `crates/office-daemon/src/main.rs`
- `crates/office-daemon/src/handlers.rs` + `handlers_test.rs`
- `crates/office-daemon/src/host.rs` + `host_test.rs`

**Scope**
- `main.rs`: `run_daemon(ext, DaemonDemo { invoke: None, driver: Some(driver_entry) })`;
  `static CMD_TX: OnceLock<Sender<Input>>`, `static KOMA_WRITE: OnceLock<Koma>` (try_clone
  for handler-safe `panel_push`), populated before `run_daemon` (fleet-board pattern).
- `impl Extension for Office`: `manifest()` from `include_str!("../../../manifest.json")`
  parsed at startup (drift fails loudly); `on_invoke` maps `tool.call` -> `Command`,
  `panel.msg` -> `Command`, returns an immediate `{output}` / `{ok,accepted}` ack;
  `on_event` maps `subagent.done`/`agent.turn_end`/`session.foreground_change` -> `HostEvent`
  and `agents.done` (via notify) -> `HostEvent::AgentsDone`.
- `host.rs`: `trait Host { fn call(&mut self,&str,Value)->Value; fn panel_push(&mut self,&str,Value); fn notify(&mut self,&str,Value); }`; `KomaHost` wrapping `Koma`; `FakeHost`
  with scripted responses + a recorded call log for driver tests. Error-value string-match
  helpers (`is_grant_denied`, `is_timeout`).

**Test plan**
- `handlers_test.rs`: a `tool.call` invoke enqueues the right `Command` and returns a valid
  ack without touching a Koma handle (assert no host call); malformed params -> error output,
  no panic; each event name maps to the right `HostEvent`.
- `host_test.rs`: `FakeHost` returns scripted values; error-classifier helpers detect
  `grant denied:` / `koma call: timed out` prefixes.
- Build: demo mode (`KOMA_EXT_SOCKET` unset) runs the scripted handshake and exits 0.

**Green commit:** `feat(daemon): SDK glue, Host trait, invoke/event handlers`

---

## W7 — Driver: kernel tick loop + effect execution + reconciliation + panel protocol freeze

**Goal:** wire kernel + store + host into the running loop; execute every `Effect`;
implement start-up reconciliation; FREEZE the panel message protocol.

**Files touched**
- `crates/office-daemon/src/driver.rs` + `driver_test.rs`
- `crates/office-daemon/src/main.rs` (driver_entry hookup)
- `docs/PANEL_PROTOCOL.md` (frozen copy of ARCHITECTURE.md 10.2 for the UI waves)

**Scope** (ARCHITECTURE.md 5.1, 9.1, 10.2)
- `driver_entry(&mut Koma)`: build `KomaHost`, load store (self-healing adopt/quarantine, W5),
  bind session via `sessions.list` (2.2), acquire leases, run W9-less bootstrap; spawn the
  DEDICATED lease-heartbeat thread (own `try_clone` handle) so a slow host call can't age the
  lease (4.4, 5.1); own a per-bound-session `SessionCapacity` token and pass remaining slots
  into `kernel::step`; loop on `recv_timeout(1s)` -> `Tick`; feed `Input` to `kernel::step`;
  execute effects:
  - `Spawn` -> `sessions.spawn_into`/`agents.spawn` (bound-session logic 2.2), record binding
    with `spawned_at_ms`, decrement capacity token, persist before next effect. Before firing a
    ready set, probe bound-session liveness via `sessions.list` and short-circuit the whole set
    on a `{status:"sent"}` (cross-process) reply — release lease, no untracked duplicates (5.6).
  - `Kill` -> `agents.kill`; `FetchResult` -> `agents.result` -> feed `HostEvent::Result`.
  - `InvokeModel` -> dispatch to the bounded invoke worker pool (own `try_clone` handles, default
    2), NOT inline; result returns as `Command::InvokeResult` on `CMD_TX` (full flow in W9).
  - `PublishContext` -> `context.set`; `QueueChatPrompt` -> `chat.prompt` with 6.5 outbox
    discipline (budget/queue-full/dup handling via error strings).
  - `PanelPush` -> `digest::panel_snapshot` + 900KB size guard + 250ms throttle ->
    `panel_push("board", ...)`.
  - `EnsureDesk` -> `std::fs` create under workspace; `Persist` -> store save.
- Reconciliation (9.1): on start, `agents.status` on stale bindings; live terminal `status` ->
  result path; the ERROR values `unknown agentId: N` / `session closed` -> killed path (NOT a
  `status` field); binding older than the runtime ceiling -> force-kill + re-queue;
  `agents.list` orphan sweep (this is where `status:"gone"` is consumed) -> `agents.kill`.
- Panel handlers (`hello`/`state`/`prd_get` sync reply with snapshot inline; mutating ops
  enqueue Commands + push results). Handlers stay off any blocking call (100ms oneshot budget)
  — safe now that invokes and the heartbeat run off the driver.

**Test plan (`driver_test.rs`, FakeHost-scripted)**
- one dispatch cycle: Tick -> ready task -> FakeHost records a `spawn_into` with the right
  agent/model/notify, binding persisted.
- completion cycle: feed `agents.done` -> driver calls `agents.result` (scripted report) ->
  task -> Review -> reviewer spawned.
- reconcile: a binding whose scripted `agents.status` returns the error value
  `{"error":"unknown agentId: N"}` (and separately `{"error":"session closed"}`) -> task
  re-queued via the killed path; a scripted `agents.list` entry with `status:"gone"` not
  referenced by any task -> `agents.kill` orphan sweep. Assert the driver does NOT key off a
  `status` field on the `agents.status` reply.
- runtime ceiling: a binding with a `spawned_at_ms` older than `worker_max_runtime_ms` (clock
  advanced in FakeHost) -> driver issues `agents.kill` + re-queues, no liveness check needed.
- steal safety: with the bound session absent from a scripted `sessions.list`, the driver
  short-circuits dispatch on the first `{status:"sent"}` and releases the lease instead of
  firing the rest of the ready set.
- outbox discipline: `chat.prompt` scripted to return `queue full (5)` -> notice retried;
  `turn budget exhausted` -> outbox paused until an `agent.turn_end` event.
- panel push size guard: oversized snapshot -> summary mode + `truncated:true`.

**Green commit:** `feat(daemon): kernel tick loop, effect exec, reconciliation; freeze panel protocol`

---

## W8 — File inbox bridge (daemon-mode reach path)

**Goal:** the documented workaround for contributed tools being invisible in `--daemon`
sessions (Limitation 1): a workspace file inbox the main chat writes to.

**Files touched**
- `crates/office-daemon/src/inbox.rs` + `inbox_test.rs`
- `crates/office-daemon/src/driver.rs` (poll hook each tick)
- `crates/office-core/src/digest.rs` (context blob already advertises the inbox path — verify)

**Scope** (ARCHITECTURE.md 6.4)
- Watch `<workspace>/koma-workflow/inbox/` each tick; parse `<millis>-<slug>.json`
  `{op, ...}` (brief/status/authorize/interrupt/resume/comment); move consumed files to
  `inbox/processed/`; map to `Command`s; answer via `chat.prompt` + panel.
- Tolerant: malformed file -> move to `inbox/rejected/` with an error note, never crash.
- Bound the poll (max N files/tick) to avoid a flood stalling the tick.

**Test plan (`inbox_test.rs`, tempdir)**
- a valid `brief` file -> right `Command`, file moved to processed.
- malformed JSON -> moved to rejected, no panic.
- each op parses to the correct command; unknown op -> rejected.
- flood cap: >N files -> only N consumed this tick, rest remain.

**Green commit:** `feat(daemon): workspace file-inbox bridge for daemon-mode sessions`

---

## W9 — Front-office persona (models.invoke folding + PRD/breakdown/authorization)

**Goal:** the LLM persona and the PRD -> breakdown -> authorize flow, all off the control
loop, with folded multi-turn on the single-shot invoke API.

**Files touched**
- `crates/office-core/src/office.rs` + `office_test.rs` (persona prompt assembly, folding
  policy, breakdown JSON parse+validate)
- `crates/office-daemon/src/driver.rs` (invoke execution + retry)
- `crates/office-core/src/kernel.rs` (Command handling for brief/authorize/breakdown-accept)

**Scope** (ARCHITECTURE.md 6.2, 6.3)
- `office::build_invoke(project, new_user_msg) -> (system, prompt)` with transcript fold when
  assembled prompt > 24KB (emit a summarize-invoke request as an Effect), hard-truncate at
  32KB as a guard.
- `office::parse_breakdown(&str) -> Result<Breakdown, ParseErr>`: JSON contract -> epics/
  stories/tasks with blocked_by, acceptance, priorities; validate slug uniqueness + DAG
  acyclic (graph.rs) + non-empty acceptance; one re-ask on parse failure.
- Authorization: delivery-path hard gate (absolute, mkdir -p, inside-workspace check unless
  `allow_outside_workspace`); phase Ready->Running only when valid.
- Driver: the kernel emits `InvokeModel` effects; the driver runs them on the bounded invoke
  worker pool (default 2 concurrent, each owns a `try_clone`'d `Koma`), NEVER inline on the tick
  loop (5.1) — a 25-125s PRD flow must not freeze dispatch/reconcile/heartbeat/panel-reads.
  role=config.office_role; one retry on timeout (re-emit `InvokeModel`); surface `unknown role`/
  route errors verbatim. Results feed back as `Command::InvokeResult { req_id, ... }` on `CMD_TX`;
  the kernel matches them to the pending request. This is the concrete meaning of "NO LLM in the
  control loop": the kernel emits invoke requests and consumes results as ordinary commands.

**Test plan (`office_test.rs`)**
- fold triggers over 24KB and preserves the newest turns + summary slot; hard cap never
  exceeded.
- breakdown parse: valid JSON -> validated board; cyclic deps rejected; duplicate slugs
  rejected; empty acceptance rejected; malformed -> re-ask path.
- authorization: missing delivery path blocks Running; relative path rejected; outside
  workspace blocked unless escape hatch.
- driver off-loop (`driver_test.rs`): a FakeHost `models.invoke` scripted to block feeds the
  result back as `Command::InvokeResult` while the tick loop keeps servicing `Tick`/panel-read
  inputs in the meantime (assert dispatch and a `hello` reply are not starved during the invoke);
  timeout -> exactly one re-emitted `InvokeModel`; pool cap honored (no more than N in flight).

**Green commit:** `feat(office): models.invoke persona folding + PRD/breakdown/authorize`

---

## WU1 — Panel: Vite scaffold + bridge + dashboard

**Goal:** buildable panel served at `koma://extension/...`, rehydrate via `hello`, live
dashboard from snapshot pushes. Consumes the protocol frozen in W7.

**Files touched**
- `ui/package.json`, `ui/vite.config.ts` (`base:'./'`), `ui/index.html`,
  `ui/public/koma-panel.js` (verbatim from SDK sample)
- `ui/src/main.tsx`, `ui/src/bridge.ts` (KomaPanel wrapper: send + onPush + seq handling),
  `ui/src/store.ts` (snapshot state), `ui/src/views/Dashboard.tsx`
- `ui/tsconfig.json`, Tailwind config

**Scope**
- Bridge: `hello` on load -> snapshot; `onPush` replaces state (full snapshots, never
  deltas — 10.3); reconnect/rehydrate on visibility change.
- Dashboard: multi-project cards (phase badge, done/total ring via framer-motion, running
  count, parked count, spawn-count burn proxy, last notice, global halt indicator).
- Relative asset URLs verified (build to `ui/dist`, grep for absolute `/assets`).

**Test plan**
- `npm run build` green; a tiny bridge unit test (vitest) for envelope shape + seq handling
  against a mocked `window.parent.postMessage`.
- manual: load `dist/index.html` with a stub push; cards render.

**Green commit:** `feat(ui): vite scaffold, panel bridge, multi-project dashboard`

---

## WU2 — Panel: board + drilldown + dependency map

**Files touched**
- `ui/src/views/Board.tsx`, `ui/src/views/Drilldown.tsx`,
  `ui/src/components/DepMap.tsx` (inline SVG), `ui/src/components/Card.tsx`

**Scope**
- Board: five columns; Parked cards in Review with amber badge; drag between legal columns
  (emits `card_move`, kernel guards); running cards show agent id + activity pulse;
  blocked-by chips.
- Drilldown: project -> epic -> story -> task tree with rollups.
- DepMap: topo-sorted SVG lanes; parked nodes highlighted; halt-culprit path in red.
- Interrupt(hard)/Drain(soft)/Resume buttons with confirm.

**Test plan**
- `npm run build` green; vitest for the topo-lane layout function (pure) and legal-move
  guard; manual drag smoke test against a stub daemon.

**Green commit:** `feat(ui): board, drilldown, SVG dependency map, interrupt controls`

---

## WU3 — Panel: task detail + comments/receipts + PRD viewer + office chat

**Files touched**
- `ui/src/views/TaskDetail.tsx`, `ui/src/views/Prd.tsx`,
  `ui/src/views/OfficeChat.tsx`, `ui/src/components/Comments.tsx`

**Scope**
- Task detail: description, acceptance checklist, state history, attempts/bounces, worker
  report + review verdict (monospace), comments thread with receipt pills
  (pending/delivered/read + timestamps), unpark, kill-worker, priority editor. Detail body
  fetched on demand via `task_detail` (10.2) to keep pushes small.
- PRD viewer: rendered markdown, read-only, office-chat pane beside it during Drafting.
- Office chat: transcript, folding indicator, outbox/notice log with sent/paused states;
  sends `office_chat` (async; reply via push/chat).

**Test plan**
- `npm run build` green; vitest for the receipt-pill state mapping and markdown render
  sanitization; manual: comment add shows pending -> delivered -> read as stub advances.

**Green commit:** `feat(ui): task detail, comments/receipts, PRD viewer, office chat`

---

## WU4 — Panel: settings sidebar + polish

**Files touched**
- `ui/src/views/Settings.tsx`, `ui/src/theme.ts`, animation/polish across views

**Scope**
- Per-project model bindings (free-text slugs, blank = inherit Main), max workers, bounce
  budget, keep-desks toggle, state-root path display -> `config_set`.
- Theme (light/milk + dark), reduced-motion honoring, empty states, error toasts for
  `grant denied` / lease read-only mode.

**Test plan**
- `npm run build` green; vitest for the config form validation (clamp max_workers 1..=4);
  manual full walkthrough against a stub daemon.

**Green commit:** `feat(ui): settings sidebar, theming, polish`

---

## W10 — Packaging + dev install + end-to-end

**Goal:** ship a zip that installs and runs; document the dev loop; live end-to-end smoke.

**Files touched**
- `pack.sh`, `dev-install.sh`
- `docs/DEV.md` (dev-install + demo-mode + live-test steps)
- `Cargo.toml` / CI config if any (release profile)

**Scope** (ARCHITECTURE.md 12)
- `pack.sh`: `cargo build --release -p office-daemon` + `cd ui && npm run build`; stage
  `manifest.json` (runtime.exec -> `bin/office-daemon`), `bin/office-daemon`, `ui/` (Vite
  dist) -> `dist/workflow.zip` (SDK pack.sh layout).
- `dev-install.sh`: unpack into `~/.koma/extensions/aula.workflow/`, upsert
  `installed_extensions` entry into `~/.koma/config.json` via jq, prompt restart. Never
  touches the simple-coders tree.
- End-to-end smoke (manual, documented): install, restart koma, open the Workflow tab
  (auto-start via first panel.msg), create a project, author a trivial PRD, set a delivery
  path inside the workspace, authorize, watch one task grind Todo->done, add a comment and
  see the receipt flip, Interrupt/Resume, kill koma and confirm reconcile resumes.

**Test plan**
- `cargo build --workspace` + all `*_test.rs` green; `pack.sh` produces a zip with the
  correct layout (assert `manifest.json`, `bin/office-daemon`, `ui/index.html` present via a
  shell check). The end-to-end is manual (host is a live daemon; no automated harness).

**Green commit:** `chore(release): pack.sh, dev-install, end-to-end smoke docs`

---

## Wave-to-requirement traceability

| Requirement (concept / task) | Wave(s) |
|---|---|
| PROJECT/EPIC/STORY/TASK + columns + blocked-by DAG | W1, W2 |
| Task/project state machines (running/interrupted/halted/done) | W2, W4 |
| Deterministic kernel dispatch, review pipeline, bounce/park/halt | W4 |
| Interrupt/resume (hard-kill default + soft drain) | W4, W7 |
| Durable store, atomic writes, versioned schema, survives both restarts | W5 |
| Two-sessions-racing lease | W5, W7 |
| Front office persona (models.invoke folded multi-turn) | W9 |
| Contributed tools + daemon-mode inbox fallback + context.publish + chat.prompt | W6, W7, W8, W9 |
| Worker/reviewer prompt engineering + report protocol | W3 |
| Desks (ext-managed dirs, human-readable koma-workflow marker, cleanup) | W4 (EnsureDesk), W7 |
| Panel dashboard/board/drilldown/detail/comments/PRD/controls | WU1–WU4 |
| Live updates within push caps (full-snapshot + resync) | W7, WU1 |
| Failure matrix behaviors (reconcile, missed events, budget) | W7, W9 |
| Packaging + dev loop + limitations | W10, docs |
