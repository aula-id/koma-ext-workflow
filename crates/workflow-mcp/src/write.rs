//! Where a `workflow-mcp` command tool writes its inbox file, and how the file is named.
//!
//! The command tools (brief/authorize/comment/interrupt/resume) never talk to the daemon
//! directly. They drop a JSON file into the SAME file-inbox pipeline the daemon already
//! consumes (ARCHITECTURE.md 6.4) — so a chat or agent that can reach this MCP server
//! reaches the office through one, already-tested code path. This module resolves WHICH
//! inbox directory a call targets and mints a unique, monotonic filename for the drop.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

/// A workspace-relative inbox lives under `<base>/koma-workflow/inbox`, matching the
/// directory the daemon's per-workspace poll watches (driver.rs `poll_inbox`).
fn workspace_inbox(base: &Path) -> PathBuf {
    base.join("koma-workflow").join("inbox")
}

/// Pure inbox-dir resolution, given every input explicitly so the decision matrix is
/// unit-testable without touching the real env / cwd / filesystem. Order (first match wins):
///
/// 1. an explicit `workspace` tool arg                       -> `<ws>/koma-workflow/inbox`
/// 2. `$WORKFLOW_WORKSPACE`                                   -> `<env>/koma-workflow/inbox`
/// 3. the process cwd IF it already has a `koma-workflow/` dir -> `<cwd>/koma-workflow/inbox`
/// 4. the GLOBAL fallback (the workflow home's inbox)         -> `<global_inbox>` verbatim
///
/// Blank (empty/whitespace) `workspace`/env values are ignored so an accidental empty
/// string never shadows the later, more specific fallbacks. Note the asymmetry: the
/// workspace-relative cases append `koma-workflow/inbox` (so the daemon's WORKSPACE poll
/// finds them), while the global fallback is used verbatim (it is already the workflow
/// home's `inbox`, which the daemon's GLOBAL poll watches).
pub fn resolve_inbox_dir(
    workspace_arg: Option<&str>,
    env_workspace: Option<&str>,
    cwd: &Path,
    cwd_has_marker: bool,
    global_inbox: &Path,
) -> PathBuf {
    if let Some(ws) = workspace_arg.map(str::trim).filter(|s| !s.is_empty()) {
        return workspace_inbox(Path::new(ws));
    }
    if let Some(ws) = env_workspace.map(str::trim).filter(|s| !s.is_empty()) {
        return workspace_inbox(Path::new(ws));
    }
    if cwd_has_marker {
        return workspace_inbox(cwd);
    }
    global_inbox.to_path_buf()
}

/// Runtime wrapper over [`resolve_inbox_dir`]: reads `$WORKFLOW_WORKSPACE`, the process cwd
/// (and whether it already has a `koma-workflow/` dir), and the global inbox
/// (`office_store::root()/inbox` — the SAME dir the daemon's global poll watches, so the MCP
/// writer and the daemon reader always agree, and `$WORKFLOW_HOME` is honored by both).
pub fn inbox_dir_for(workspace_arg: Option<&str>) -> PathBuf {
    let env_workspace = std::env::var("WORKFLOW_WORKSPACE").ok();
    let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    let cwd_has_marker = cwd.join("koma-workflow").is_dir();
    let global_inbox = office_store::root().join("inbox");
    resolve_inbox_dir(
        workspace_arg,
        env_workspace.as_deref(),
        &cwd,
        cwd_has_marker,
        &global_inbox,
    )
}

/// Process-global monotonic counter that disambiguates two files minted in the same
/// millisecond, so filenames are unique and strictly increasing within a run.
static COUNTER: AtomicU64 = AtomicU64::new(0);

fn unix_millis() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// Mint the next inbox filename: `<unix-millis>-<counter>-mcp.json`. The `-mcp` tag marks
/// the writer; the 6-digit zero-padded counter keeps same-millisecond drops
/// lexicographically ordered the way the daemon's filename sort expects.
pub fn next_inbox_filename() -> String {
    let millis = unix_millis();
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    format!("{millis}-{n:06}-mcp.json")
}

/// Write one inbox command file (creating the inbox dir if needed) and return the full path
/// written. `body` is an `office_core::inboxmsg` builder output. Any IO/serialization error
/// is returned for the caller to fold into a tool result.
pub fn write_inbox_file(dir: &Path, body: &serde_json::Value) -> std::io::Result<PathBuf> {
    std::fs::create_dir_all(dir)?;
    let path = dir.join(next_inbox_filename());
    let bytes = serde_json::to_vec(body)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
    std::fs::write(&path, bytes)?;
    Ok(path)
}

#[cfg(test)]
#[path = "write_test.rs"]
mod write_test;
