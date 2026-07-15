# Workflow — Panel Message Protocol (FROZEN, W7)

This is the frozen copy of the panel <-> daemon message contract (ARCHITECTURE.md 10.2),
pinned by W7 so the UI waves (WU1-WU4) can build against a stable surface. The Rust driver
(`crates/office-daemon/src/driver.rs`) and the panel bridge (`ui/src/bridge.ts`) MUST agree
on exactly this shape. Do not change it without bumping the UI waves together.

Schema/branding invariants: panel id is `"board"`, panel title is `"Workflow"`, the push
envelope `kind` is `"snapshot"`, and every state file the daemon persists carries
`"schema": "workflow/1"`.

---

## 1. Panel -> daemon (`panel.msg`)

The host delivers a panel message to the extension's `on_invoke` as
`{ panelId: "board", payload: <op object> }` with a **10s host timeout**. Handlers answer
fast and NEVER call back into the host (`Koma::call`) on that thread (the SDK deadlock rule).

### 1.1 Synchronous reads (reply carries data INLINE)

These three ops answer immediately from the daemon's in-memory snapshot cache — no round trip
to the tick loop, no host call. The reply direction has no 256KiB cap (that cap is
panel->host only) and rides the 4MiB wire frame; the snapshot builder drops to summary mode
long before that limit.

```
{ op: "hello", uiVersion? }          -> { ok: true, snapshot: <Envelope> }   // rehydrate on load
{ op: "state", project? }            -> { ok: true, snapshot: <Envelope> }   // full board (client filters)
{ op: "prd_get", project }           -> { ok: true, prd: <string|null> }     // rendered PRD markdown
```

`<Envelope>` is byte-for-byte the same shape the daemon pushes (section 2). When the daemon
has not finished booting, `hello`/`state` return an empty envelope
(`{ kind: "snapshot", seq: 0, projects: [] }`) and the panel simply waits for the next push.

### 1.2 Mutating ops (enqueue a Command, ack immediately, result arrives as a PUSH)

Each returns `{ ok: true, accepted: true }` right away; the board mutation (if any) is applied
on the kernel tick loop and surfaced by a subsequent full-snapshot push.

```
{ op: "office_chat", project, message }               // async; office reply arrives via push/chat
{ op: "authorize", project, deliveryPath, allowOutsideWorkspace? }
{ op: "interrupt", project, mode }                    // "hard" | "soft" (default hard)
{ op: "resume", project }
{ op: "card_move", task, to, killWorker? }            // column move; kernel guards illegal moves
{ op: "comment_add", task, text }
{ op: "unpark", task }
{ op: "edit_task", task, ... }                        // opaque patch
{ op: "edit_deps", task, ... }                        // opaque patch
{ op: "config_set", project, maxWorkers?, bounceBudget?, workerModel?, reviewerModel?, keepDesks? }
{ op: "project_create", name }
{ op: "project_archive", project }
{ op: "task_detail", task }                           // on-demand detail (full snapshot already carries it)
```

Malformed payloads (missing required field, unknown `op`) never crash a handler: they return
`{ error: "<reason>" }`.

---

## 2. Daemon -> panel (`panel_push("board", Envelope)`)

The daemon pushes ONLY full-state snapshots, never deltas. Pushes are lossy at two layers
(256-entry drop-oldest daemon outbox; fire-and-forget GUI post) and `panel_push` silently
drops payloads over 1MiB — full snapshots make every drop harmless because the next one
replaces state entirely.

```
Envelope = {
  kind: "snapshot",
  seq: <u64>,             // monotonic; a gap is irrelevant (snapshot replaces state)
  truncated: <bool>,      // true when the size guard forced summary mode
  projects: [ Project ]
}
```

- **Size guard**: if the serialized full-mode envelope exceeds ~900KB, the daemon rebuilds it
  in per-project summary mode (drops report/history/comment bodies, keeps counts + states) and
  sets `truncated: true`. The panel then fetches missing bodies via `{ op: "task_detail" }`.
- **Throttle**: at most one push per 250ms; dirty flags coalesce in between.

### 2.1 `Project` (full mode)

```
{
  id, name,
  phase: { kind: "drafting"|"ready"|"running"|"interrupted"|"halted"|"done",
           reason?  (halted),  atMs?  (done) },
  deliveryPath: <string|null>,
  seq: <u64>,
  tasks: [ Task ],
  prdMarkdown, officeTranscript: [ { who: "user"|"office", text } ], officeSummary,
  trdMarkdown, researchNotes,   // 6.2b Drafting pipeline docs; full mode only, drop in summary
  researchActive: <bool>, auditActive: <bool>,   // fixed-staff liveness for the office view; ADDITIVE
  config: { maxWorkers, bounceBudget, workerModel: <string|null>,
            reviewerModel: <string|null>, keepDesks: <bool> }   // config_set round-trip
}
```

`trdMarkdown` (Technical Requirements Document) and `researchNotes` (web-research findings) are
authored in the Drafting pipeline (PRD -> research -> TRD -> breakdown, ARCHITECTURE.md 6.2b).
Both are full-mode only and are dropped in summary mode, exactly like `prdMarkdown`. Additive:
older panels ignore the extra keys; the schema stays `workflow/1`.

### 2.2 `Task` (full mode; summary mode omits the body fields)

```
{
  id, title,
  column: "backlog"|"todo"|"onprogress"|"review"|"done",   // Parked renders in "review"
  state:  "backlog"|"todo"|"onprogress"|"review"|"parked"|"done",
  priority, blockedBy: [taskId], bounces,
  // full mode only:
  description, acceptance: [string],
  comments: [ { id, author: "user"|"office"|"system", text, createdMs,
                receipt: { state: "pending"|"delivered"|"read", atMs? } } ],
  lastReport, lastReview, history: [ { atMs, event } ],
  persona?   // office-view desk label (short worker name, e.g. "nova"); ADDITIVE
}
```

`persona` (full mode) is the short worker persona at the task's desk — present while the task is
`onprogress` / `review` / `parked` (the desk is occupied), omitted otherwise. It drives the pixel
office view (ARCHITECTURE.md 5.2b / 10.3) and is additive: older panels ignore it, schema stays
`workflow/1`.

A comment still `pending` when its task reaches `done` is surfaced verbatim (state stays
`pending`) so the panel can flag it "never delivered — reopen to send"; the daemon never
forges a `read` receipt.

---

## 3. Bridge rules (for the UI waves)

- Send `hello` on load and on `visibilitychange` -> visible; replace local state with
  `reply.snapshot`.
- Treat every `onPush` envelope as a full replacement of state; never merge deltas.
- `seq` is advisory (gap detection is unnecessary since snapshots are total).
- All panel->daemon traffic goes through `KomaPanel.send` (15s timeout, above the host's 10s
  panel.msg budget so real host errors surface) and `KomaPanel.onPush`.
</content>
</invoke>
