# Workflow — Development Guide

This guide covers local development, testing, and deployment of the Workflow extension.

## Project Structure

```
/media/wangsa/project-x/agentic-kanban/
  manifest.json               # Extension manifest (single source of truth)
  Cargo.toml                  # Rust workspace
  pack.sh                     # Build release binary and Vite UI -> dist/workflow.zip
  dev-install.sh              # Unpack zip into ~/.koma/extensions/aula.workflow/
  docs/
    ARCHITECTURE.md           # Design and implementation details
    BUILD_WAVES.md            # Wave-by-wave implementation plan
    PANEL_PROTOCOL.md         # Frozen panel message protocol
  crates/
    office-core/              # Pure domain + kernel (no IO)
    office-store/             # Durable store + lease
    office-daemon/            # The shipped binary (daemon kind)
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

# 2. Install into ~/.koma/extensions/aula.workflow/
./dev-install.sh

# 3. Restart koma:
#    - If running GUI: restart the Workflow extension via the admin panel, or kill/restart koma
#    - If running TUI: quit and restart

# 4. Open the Workflow tab (appears as "Workflow" in the panel bar)
```

After installation, the extension state root is at `~/.koma-workflow/` (outside the install dir, so upgrades don't wipe state).

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
   - On "write the PRD", the office drafts markdown sections

6. **Accept the breakdown**:
   - Panel: see the auto-generated epics, stories, tasks
   - You can edit structure before going live

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
