# Workflow — Development Guide

This guide covers local development, testing, and deployment of the Workflow extension.

## Project Structure

```
/media/wangsa/project-x/agentic-kanban/
  manifest.json               # Extension manifest (single source of truth)
  Cargo.toml                  # Rust workspace
  pack.sh                     # Build release binary and Vite UI -> dist/workflow.zip
  dev-install.sh              # Prefer 'koma ext install --dev'; else unpack into ~/.koma/extensions/aula.workflow/
  docs/
    ARCHITECTURE.md           # Design and implementation details
    BUILD_WAVES.md            # Wave-by-wave implementation plan
    PANEL_PROTOCOL.md         # Frozen panel message protocol
  crates/
    office-core/              # Pure domain + kernel (no IO) + inbox-command builders
    office-store/             # Durable store + lease
    office-daemon/            # The shipped binary (daemon kind)
    workflow-mcp/             # Stdio MCP server: typed tools over the inbox pipeline
  ui/                         # React 19 + TS + Tailwind + Vite
```

## Building from Source

### Prerequisites

- Rust 1.70+ with `cargo`
- Node.js 24+ (managed by nvm)
- jq (for JSON manipulation in pack.sh, or python3 as fallback)
- Standard Unix tools: `zip`, `unzip`, `mkdir`, etc.

### Full Build (release)

To build the extension and package it into a distributable zip:

```bash
cd /media/wangsa/project-x/agentic-kanban
./pack.sh
```

This produces `dist/workflow.zip` containing:
- `manifest.json` with runtime.exec pointing to `bin/office-daemon`
- `bin/office-daemon` (release binary)
- `bin/workflow-mcp` (release binary; the stdio MCP server)
- `ui/` (Vite-built dashboard)

### Building Individual Crates

**Rust crates** (debug mode):

```bash
cargo build --workspace
cargo test --workspace
```

Warnings are expected and harmless (unused code in test helpers). All tests must pass:

```bash
$ cargo test --workspace 2>&1 | grep "test result:"
test result: ok. 24 passed; 0 failed; 0 ignored
```

**UI** (debug mode):

```bash
cd ui
nvm use 24
npm install
npm run dev
# or for production build:
npm run build
```

TypeScript must pass:

```bash
npx tsc --noEmit
```

## Development Installation (Local Machine)

To install the extension into your local koma for testing:

```bash
# 1. Build and package
./pack.sh

# 2. Install. When koma is on PATH, dev-install.sh uses koma's official dev sideload verb
#    (koma ext install --dev dist/workflow.zip): it unpacks into ~/.koma/extensions/aula.workflow/,
#    auto-grants every `requires` (tier "dev", enabled), and replaces the same id in place. When
#    koma is NOT on PATH it falls back to the manual unzip + installed_extensions registry edit.
./dev-install.sh

# 3. Restart koma — ALREADY-RUNNING sessions must be restarted to pick up a new build:
#    - GUI: restart the Workflow extension via the admin panel, or kill/restart koma
#    - TUI: quit and restart

# 4. Open the Workflow tab (appears as "Workflow" in the panel bar)
```

After installation, the extension state root is at `~/.koma-workflow/` (outside the install dir, so upgrades don't wipe state). `manifest.json` also declares this as the extension's `workspace_dir` (koma 0.3.0) — koma's host-side file-tool containment root.

`dev-install.sh` upserts an `.mcp_servers` entry (name `workflow`) into `~/.koma/config.json` pointing at `bin/workflow-mcp`, preserving an existing entry's `uuid`. This runs in BOTH install paths, because `koma ext install --dev` registers the extension + grants but NOT the MCP server.

## MCP tools

Alongside the daemon, the extension ships a second binary, `bin/workflow-mcp`: a stdio MCP
server that is a TYPED FRONT DOOR to the office. Because `dev-install.sh` registers it under
`.mcp_servers`, koma's MCP client spawns it and advertises its tools to the model as
`mcp__workflow__workflow_*` — usable even in `--daemon` sessions where contributed tools are
invisible.

Six tools:

- `workflow_brief { message, project?, workspace? }` — start/continue the PRD conversation; a new/unknown project id mints a project.
- `workflow_status { project? }` — READ-ONLY board digest, returned inline (never writes).
- `workflow_authorize { project, delivery_path, workspace? }` — approve the PRD, start the line.
- `workflow_comment { task, text, workspace? }` — comment on a task.
- `workflow_interrupt { project, hard?, workspace? }` — interrupt (`hard` default true).
- `workflow_resume { project, workspace? }` — resume an interrupted project.

The five COMMAND tools don't talk to the daemon directly: they write a JSON file into the
SAME file inbox the daemon already consumes (ARCHITECTURE.md 6.4) using the shared
`office_core::inboxmsg` builders, so acks and replies come back as CHAT NOTICES, not in the
tool result. `workflow_status` reads the store directly and returns the digest inline.

Inbox directory resolution for a command tool (first match wins):

1. an explicit `workspace` tool arg → `<ws>/koma-workflow/inbox`
2. `$WORKFLOW_WORKSPACE` → `<env>/koma-workflow/inbox`
3. the process cwd IF it already has a `koma-workflow/` dir → `<cwd>/koma-workflow/inbox`
4. the global fallback → `~/.koma-workflow/inbox`

Files are named `<unix-millis>-<counter>-mcp.json`. The daemon polls BOTH the per-workspace
inbox and (added with this server) the global `~/.koma-workflow/inbox`; on the global inbox it
claims only files addressed to a project it owns — plus new-project briefs (mint locally) and
undeterminable-malformed files — and leaves everything else for the owning instance, with a
race-safe atomic-rename claim.

## Demo Mode (No Live koma)

For testing the core logic without a running koma daemon:

```bash
# Build with demo mode enabled (no live Koma socket)
unset KOMA_EXT_SOCKET
cargo run -p office-daemon
```

Demo mode runs a scripted handshake and exits cleanly. Useful for:
- Verifying the extension starts and loads the manifest
- Quick smoke test without a full koma setup

## Testing

### Unit Tests (Deterministic)

All business logic is tested in `*_test.rs` files beside the module:

```bash
cargo test --workspace -- --nocapture
```

Key test suites:
- `crates/office-core/src/domain_test.rs` — domain model (serialization, state machines)
- `crates/office-core/src/kernel_test.rs` — dispatch, review, bounce/park/halt (main logic)
- `crates/office-store/src/store_test.rs` — atomic writes, versioning, crash tolerance
- `crates/office-store/src/lease_test.rs` — two-session racing, concurrent comments

### Integration Test (Manual, Live)

The end-to-end smoke test requires a running koma daemon. Follow this sequence:

1. **Install the extension** (see Development Installation above)

2. **Start a koma GUI session**:
   ```bash
   koma gui
   ```

3. **Open the Workflow panel** — click the "Workflow" tab to auto-start the extension

4. **Create a project**:
   - Use the panel `New Project` button or the contributed tool `workflow_brief`
   - Message: "Create a project to test the Workflow extension"

5. **Author a PRD** (dialogue with the office):
   - The office persona asks clarifying questions
   - On "write the PRD", the office drafts markdown sections (a ```prd fenced block)

6. **Watch the research -> TRD -> breakdown pipeline** (ARCHITECTURE.md 6.2b):
   - Capturing the PRD auto-spawns the `office-researcher` (web-researches the stack) — watch
     the chat notice "researching the stack before the TRD"
   - Research findings land, the office drafts the TRD (```trd), then the breakdown runs
   - Panel `docs` tab: see the PRD, the Technical Requirements, and the collapsed research notes
   - Panel board: see the auto-generated epics, stories, tasks; you can edit structure before go
   - Degradation is graceful: if research or the TRD call fails, drafting continues from the PRD
     alone and the board still fills (nothing wedges Drafting)

7. **Authorize and run**:
   - Panel: set a delivery path inside the session workspace (e.g., `~/koma-workflow-test/delivery`)
   - Panel: click Authorize (or use `workflow_authorize` tool)
   - Watch tasks progress Todo → OnProgress → Review → Done

8. **Test comments and receipts**:
   - While a task is OnProgress, use the panel or `workflow_comment` to add a comment
   - Watch the receipt transition: Pending → Delivered → (when task re-spawns or agent acks) Read

9. **Test interruption**:
   - Panel: click Interrupt (hard mode, kills running agents)
   - Tasks return to Todo, dispatch resumes on Resume

10. **Kill and reconcile**:
    - Kill koma (`Ctrl+C` on the CLI)
    - Start koma again
    - Open the Workflow panel — the extension auto-starts, reconciles, and resumes

All steps must succeed and state must be durably preserved.

## Common Development Tasks

### Updating the Domain Model

If you modify `crates/office-core/src/domain.rs`:

1. Update the corresponding test in `domain_test.rs`
2. Run `cargo test crate::domain_test`
3. Ensure serialization (serde) round-trips correctly
4. Bump `SCHEMA_V` in `domain.rs` if the on-disk format changes
5. Add a migration in `crates/office-store/src/store.rs` if needed

### Updating the Kernel Logic

If you modify `crates/office-core/src/kernel.rs`:

1. Add or update tests in `kernel_test.rs`
2. Run `cargo test crate::kernel_test`
3. Every Effect case must be exercised and verified deterministic

### Updating the Panel

If you modify files in `ui/src/`:

1. Run `npm run build` to verify TypeScript and bundle
2. Manually test in koma GUI: install the extension and check the UI renders
3. Use browser DevTools (F12) to debug; the panel is a standard React app

**Standalone screenshots (no live koma)** — run `npm run dev` and open with `?mock=1` plus a
`?view=` deep link for a deterministic initial view (see `App.tsx parseDeepLink`):

- `?mock=1&view=office-map` — the pixel virtual office (default project view; picks the running
  mock project, so the personas/desks render deterministically)
- `?mock=1&view=board` / `&view=drilldown` / `&view=depmap` — board sub-views
- `?mock=1&view=office` — the PRD/docs tab (office chat), on the drafting mock project
- `?mock=1&view=task` — a project board with a rich task drawer pre-opened
- `?mock=1&view=dashboard` / `&view=settings` — the multi-project dashboard / settings

Add `&project=<id>` to pin a specific mock project (`notif`, `loyalty`, `legacy`).

Sprites for the office view are generated (stdlib-only PNG encoder) by
`python3 ui/sprites-gen/gen_sprites.py` into `ui/public/sprites/`; re-run it and commit the PNGs
whenever a matrix or palette changes (Vite ships `public/` verbatim; `pack.sh` is unchanged).

### Updating the Manifest

If you modify `manifest.json`:

1. Verify it is valid JSON: `jq . manifest.json`
2. Ensure the schema is `koma-extension/v0`
3. Check that all required fields are present (id, name, version, kind, runtime, requires, contributes)
4. The `runtime.exec` in the packaged zip will be updated by pack.sh automatically

## Troubleshooting

### "extension turn budget exhausted" in chat

The office persona consumes a shared budget with other extensions. If you see this error:
- Wait for user activity (a message) to reset the budget
- Check that `config.office_role` is set to a valid model (default "main")
- The outbox will retry queued notices automatically

### Worker runs forever (liveTextLen never changes)

By default, workers are force-killed after 20 minutes (configurable per project as `worker_max_runtime_ms`). This is the hard backstop for runaway workers. To adjust:
- Panel: Settings > select a project > Runtime Ceiling (minutes)
- Minimum is 5 minutes; there is no way to disable the ceiling

### "grant denied: ..." error

The extension requires specific grants (agents:orchestrate, sessions:manage, etc.). If you see this:
1. Check that the extension is enabled in `~/.koma/config.json`
2. Restart koma to refresh grants
3. Check the koma admin panel for any ungranted permissions

### node_modules is huge

The UI has ~500 MB of dev dependencies. This is normal and is excluded from the final zip by using the `ui/dist` folder (Vite output).

### "workspace file inbox" not working in GUI

In `--daemon` mode (the default for the GUI in 0.2.0+), contributed tools are not visible to the model. The extension provides a documented workaround: the main chat can write JSON files to `<workspace>/koma-workflow/inbox/` to reach the office. The extension polls this directory each tick and answers via `chat.prompt`.

## koma 0.3.0 host API adoption

The extension targets koma 0.3.0 (SDK pinned via `cargo update -p koma-extension`). All of these are
additive — an older host ignores unknown manifest keys / verbs / params and the extra prompt text
(see ARCHITECTURE.md 14 for full detail):

- **Sub-agent tools** — each contributed sub-agent declares a `tools` allow-list in `manifest.json`
  (worker gets write/edit/bash; reviewer/researcher/auditor are read-only + web). Unknown names are
  dropped host-side; `task`/`task_send` are hard-excluded.
- **Recursion guard** — koma auto-inherits the human's `mcp__*` tools onto spawned agents with no
  opt-out, so every sub-agent prompt (manifest + `prompts.rs`) tells the agent NEVER to call
  `mcp__workflow__*`. Prompt-level only; there is no host lever.
- **Mid-run comment injection** — a comment on a task with a live worker/reviewer binding is pushed
  immediately via the `agents.send` verb (`Effect::InjectComment` -> success `HostEvent::CommentDelivered`);
  on error it stays Pending for the spawn-boundary fold. No retry loop.
- **`format:"json"`** — the kernel asks for JSON output on the breakdown + assume-check invokes;
  chat-completions dialects honor it, others ignore it.
- **Theme-aware panel** — the panel follows koma's live palette (`koma-panel.js` `onTheme`/`getTheme`
  -> `theme.ts applyHostPalette`); Settings shows "following koma theme" while host-themed, and keeps
  the manual dark/light toggle standalone.
- **`models.invoke` timeout** — the wire cap is 360s (broker inner ~330s), not the old 25s; long
  PRD/breakdown flows run off the driver's invoke pool so nothing blocks the tick loop.
- **Dev sideload verb** — `dev-install.sh` prefers `koma ext install --dev` (see above).
- **Host-side freebies (nothing to configure)** — koma groups the contributed sub-agents under the
  extension in its sidebar, and selects the extension socket via `KOMA_EXT_SOCKET` (a Windows named
  pipe / unix socket elsewhere) transparently in the SDK.

## Notes

- **State root**: `~/.koma-workflow/` (not per-extension-install, so upgrades preserve state)
- **Logs**: extension errors go to stdout/stderr; koma will rotate them
- **No emoji**: per house rules, the extension has zero emoji anywhere
- **Determinism**: the kernel is fully deterministic; two identical inputs always yield identical Effects
- **No LLM in control loop**: the kernel emits invoke *requests*, not blocks on model calls; the driver offloads invokes to a worker pool

## References

- `ARCHITECTURE.md` — locked design and why each decision was made
- `BUILD_WAVES.md` — wave-by-wave implementation and test plan
- `PANEL_PROTOCOL.md` — the frozen message protocol between daemon and panel UI
- `/media/wangsa/project-x/simple-coders/docs/EXTENSIONS.md` — koma extension host API (READ-ONLY)

## Release builds

`.github/workflows/release.yml` is tag-triggered only (`push: tags: ['v*']` — repo policy is no
push/PR builds). Pushing a `vX.Y.Z` tag builds and attaches 5 zips to the GitHub release, one per
target, each running `pack.sh` with `PACK_TARGET`/`PACK_PLATFORM` set:

- `workflow-windows-x64.zip` (`x86_64-pc-windows-msvc`, `windows-latest`)
- `workflow-darwin-arm64.zip` (`aarch64-apple-darwin`, `macos-latest`)
- `workflow-darwin-x64.zip` (`x86_64-apple-darwin`, `macos-latest`)
- `workflow-linux-x64.zip` (`x86_64-unknown-linux-gnu`, `ubuntu-latest`)
- `workflow-linux-arm64.zip` (`aarch64-unknown-linux-gnu`, `ubuntu-24.04-arm`, native runner)

No secrets required: the `koma-extension` crate dependency is fetched anonymously over
https (koma is open source). Store signing is deliberately NOT done in CI — release zips
are unsigned; signing happens out-of-band at store-publish time.

All 5 zips are unsigned — store-side signing, if any, happens out-of-band, not in this workflow.
