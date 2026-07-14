//! Workspace file inbox bridge (BUILD_WAVES.md W8, ARCHITECTURE.md 6.4).
//!
//! Contributed tools are invisible to the model in `--daemon` sessions (Limitation 1),
//! so the context blob (`office-core::digest::context_blob`) instructs the model to
//! write `<workspace>/koma-workflow/inbox/<millis>-<slug>.json` files instead. This
//! module is the driver-side consumer: it watches the directory, tolerantly parses
//! each file into the same daemon-level [`Command`](crate::handlers::Command) the
//! contributed tools produce, and moves the file out of the inbox so it is never
//! re-processed.
//!
//! Pure with respect to the host: everything here is plain filesystem + JSON parsing,
//! which is what keeps `inbox_test.rs` a tempdir-only test with no `Host`/`Koma`
//! dependency. The driver (`driver.rs`) owns turning an [`InboxOutcome::Accepted`]
//! into an actual kernel-routed command and an [`InboxOutcome`] (either variant) into
//! a `chat.prompt` acknowledgement (ARCHITECTURE.md 6.4/6.5).

use std::fs;
use std::path::{Path, PathBuf};

use serde_json::Value;

use crate::handlers::Command;

/// Bounds how many inbox files one driver tick consumes, so a flood of dropped files
/// can never stall a dispatch/reconcile/panel-push tick.
pub const MAX_FILES_PER_TICK: usize = 20;

/// The `processed/` and `rejected/` child directory names inbox files are moved into
/// once consumed.
const PROCESSED_DIR: &str = "processed";
const REJECTED_DIR: &str = "rejected";

/// The result of consuming one inbox file.
#[derive(Debug, Clone, PartialEq)]
pub enum InboxOutcome {
    /// Parsed into a daemon [`Command`]; `ack` mirrors the text the equivalent
    /// `tool.call` invoke would have returned (ARCHITECTURE.md 11) — the inbox has no
    /// synchronous caller to answer, so the driver sends this as a `chat.prompt`
    /// notice instead.
    Accepted {
        file: String,
        command: Command,
        ack: String,
    },
    /// Malformed JSON, an unknown `op`, or a missing/empty required field. The file
    /// was moved to `inbox/rejected/` with an `.error` sidecar carrying `reason`.
    Rejected { file: String, reason: String },
}

/// Poll `inbox_dir` for `*.json` files (sorted by filename, so the `<millis>-<slug>`
/// naming convention gives chronological order), consume up to `max_files`, and move
/// each one to `processed/` or `rejected/`. Never panics: a missing `inbox_dir`, an
/// unreadable file, or a filesystem error on the move is folded into an outcome (or,
/// for a genuinely unreadable file, still moved to `rejected/` when possible) rather
/// than propagated. Files beyond `max_files` are left untouched for the next tick.
pub fn poll(inbox_dir: &Path, max_files: usize) -> Vec<InboxOutcome> {
    let mut out = Vec::new();

    let entries = match fs::read_dir(inbox_dir) {
        Ok(rd) => rd,
        Err(_) => return out, // no inbox yet (or not created): nothing to do
    };

    let mut files: Vec<PathBuf> = entries
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| p.is_file() && p.extension().and_then(|e| e.to_str()) == Some("json"))
        .collect();
    files.sort();

    for path in files.into_iter().take(max_files) {
        let name = path
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("unnamed.json")
            .to_string();
        out.push(consume_one(inbox_dir, &path, name));
    }

    out
}

/// Read, parse, validate, and move a single inbox file.
fn consume_one(inbox_dir: &Path, path: &Path, name: String) -> InboxOutcome {
    let outcome = match fs::read_to_string(path) {
        Ok(text) => parse(&text, &name),
        Err(e) => Err(format!("could not read file: {e}")),
    };

    match outcome {
        Ok((command, ack)) => {
            move_to(inbox_dir, path, PROCESSED_DIR, &name);
            InboxOutcome::Accepted { file: name, command, ack }
        }
        Err(reason) => {
            move_to(inbox_dir, path, REJECTED_DIR, &name);
            write_error_note(inbox_dir, &name, &reason);
            InboxOutcome::Rejected { file: name, reason }
        }
    }
}

/// Best-effort move; a failed rename (e.g. the destination dir could not be created)
/// leaves the file in place, which just means it is retried next tick — never a panic.
fn move_to(inbox_dir: &Path, path: &Path, sub: &str, name: &str) {
    let dest_dir = inbox_dir.join(sub);
    if fs::create_dir_all(&dest_dir).is_err() {
        return;
    }
    let _ = fs::rename(path, dest_dir.join(name));
}

/// Sidecar `<file>.error` next to a rejected file, carrying the rejection reason.
fn write_error_note(inbox_dir: &Path, name: &str, reason: &str) {
    let dest = inbox_dir.join(REJECTED_DIR).join(format!("{name}.error"));
    let _ = fs::write(dest, reason);
}

/// Parse and validate one inbox file's JSON body into a `(Command, ack text)` pair,
/// mirroring the field contracts and ack conventions of `handlers::handle_tool_call`
/// (ARCHITECTURE.md 6.4/11): `{op, ...}` where `op` is one of `brief`/`status`/
/// `authorize`/`interrupt`/`resume`/`comment`.
fn parse(text: &str, file: &str) -> Result<(Command, String), String> {
    let value: Value = serde_json::from_str(text)
        .map_err(|e| format!("invalid JSON in {file}: {e}"))?;
    if !value.is_object() {
        return Err(format!("{file}: expected a JSON object at the top level"));
    }
    let op = match str_field(&value, "op") {
        Some(o) if !o.is_empty() => o,
        _ => return Err(format!("{file}: missing or empty 'op' field")),
    };

    match op.as_str() {
        "brief" => {
            let message = match str_field(&value, "message") {
                Some(m) if !m.is_empty() => m,
                _ => return Err(format!("{file}: op 'brief' requires a non-empty 'message'")),
            };
            let project = opt_str_field(&value, "project");
            Ok((
                Command::Brief { project, message },
                "office is thinking; answer will arrive via chat".to_string(),
            ))
        }
        "status" => {
            let project = opt_str_field(&value, "project");
            Ok((Command::Status { project }, "queued".to_string()))
        }
        "authorize" => {
            let project = match str_field(&value, "project") {
                Some(p) if !p.is_empty() => p,
                _ => return Err(format!("{file}: op 'authorize' requires a non-empty 'project'")),
            };
            let delivery_path = match str_field(&value, "delivery_path") {
                Some(p) if !p.is_empty() => p,
                _ => {
                    return Err(format!(
                        "{file}: op 'authorize' requires a non-empty 'delivery_path'"
                    ))
                }
            };
            Ok((
                Command::Authorize { project, delivery_path },
                "queued".to_string(),
            ))
        }
        "interrupt" => {
            let project = match str_field(&value, "project") {
                Some(p) if !p.is_empty() => p,
                _ => return Err(format!("{file}: op 'interrupt' requires a non-empty 'project'")),
            };
            let hard = opt_str_field(&value, "mode").as_deref() != Some("soft");
            Ok((Command::Interrupt { project, hard }, "queued".to_string()))
        }
        "resume" => {
            let project = match str_field(&value, "project") {
                Some(p) if !p.is_empty() => p,
                _ => return Err(format!("{file}: op 'resume' requires a non-empty 'project'")),
            };
            Ok((Command::Resume { project }, "queued".to_string()))
        }
        "comment" => {
            let task = match str_field(&value, "task") {
                Some(t) if !t.is_empty() => t,
                _ => return Err(format!("{file}: op 'comment' requires a non-empty 'task'")),
            };
            let ctext = match str_field(&value, "text") {
                Some(t) if !t.is_empty() => t,
                _ => return Err(format!("{file}: op 'comment' requires a non-empty 'text'")),
            };
            Ok((Command::Comment { task, text: ctext }, "queued".to_string()))
        }
        other => Err(format!("{file}: unknown op '{other}'")),
    }
}

fn str_field(v: &Value, key: &str) -> Option<String> {
    v.get(key).and_then(Value::as_str).map(str::to_string)
}

fn opt_str_field(v: &Value, key: &str) -> Option<String> {
    str_field(v, key)
}

// ---------------------------------------------------------------------------
// Global inbox: ownership-aware, race-safe claiming (ARCHITECTURE.md 6.4)
// ---------------------------------------------------------------------------
//
// The workspace inbox above is per-session: exactly one daemon owns a given
// `<workspace>/koma-workflow/inbox`, so `poll` may consume every file it finds. The GLOBAL
// inbox (`~/.koma-workflow/inbox`, where the `workflow-mcp` server drops files when no
// workspace is in scope) is SHARED across every koma instance, so a poller must claim only
// the files addressed to a project IT owns and leave the rest for the owning instance —
// and the claim must be race-safe against another process claiming the same file. The
// `poll_global` function below is that additive sibling of `poll`; the workspace path
// (`poll`/`consume_one`/`move_to`) is left byte-for-byte unchanged.

/// The addressed target of an inbox file, extracted by a cheap peek (this does NOT fully
/// validate — that stays [`parse`]'s job). Used only by [`poll_global`]: the target decides
/// WHICH instance may consume a file; the subsequent `parse` decides accept vs reject.
#[derive(Debug, Clone, PartialEq)]
pub enum Target {
    /// A `brief`: `project` is the addressed id (`None` = start a new project).
    Brief { project: Option<String> },
    /// A project-addressed op (`authorize`/`interrupt`/`resume`/`status`): the `project`
    /// id (`status` may legitimately carry `None` = a global query).
    Project { project: Option<String> },
    /// A `comment` addressed to a task id (ownership resolves via the task's project prefix).
    Task { task: String },
    /// No determinable target: unreadable, not JSON, no `op`, an unknown op, or an op whose
    /// required addressing field is absent. Any instance may claim such a file to reject it.
    Unknown,
}

/// Peek at an inbox file body's addressed [`Target`] for the global-inbox ownership
/// decision. Tolerant and allocation-cheap: anything it cannot classify is [`Target::Unknown`].
pub fn peek_target(text: &str) -> Target {
    let value: Value = match serde_json::from_str(text) {
        Ok(v) => v,
        Err(_) => return Target::Unknown,
    };
    if !value.is_object() {
        return Target::Unknown;
    }
    match value.get("op").and_then(Value::as_str) {
        Some("brief") => Target::Brief {
            project: opt_str_field(&value, "project"),
        },
        Some("authorize") | Some("interrupt") | Some("resume") | Some("status") => Target::Project {
            project: opt_str_field(&value, "project"),
        },
        Some("comment") => match opt_str_field(&value, "task") {
            Some(task) => Target::Task { task },
            None => Target::Unknown,
        },
        _ => Target::Unknown,
    }
}

/// The ownership verdict a [`poll_global`] caller returns for a peeked [`Target`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Claim {
    /// Consume it here: process if it parses, reject (with an `.error` sidecar) if malformed.
    Claim,
    /// Leave it in place for the instance that owns the addressed project.
    Leave,
}

/// Poll a GLOBAL inbox with ownership-aware, race-safe claiming. For each `*.json` file
/// (filename-sorted, capped at `max_files`): peek its [`Target`], and ask `owns` whether
/// this instance may consume it. A [`Claim::Leave`] file is left untouched for its owner.
/// A [`Claim::Claim`] file is consumed by ATOMICALLY renaming it into `processed/` (parsed
/// OK) or `rejected/` (malformed) — the rename IS the claim, so if a racing instance
/// already moved the file, this rename fails and the file is skipped silently (no outcome,
/// no panic). Returns one outcome per file THIS instance actually claimed.
pub fn poll_global<F>(inbox_dir: &Path, max_files: usize, owns: F) -> Vec<InboxOutcome>
where
    F: Fn(&Target) -> Claim,
{
    let mut out = Vec::new();

    let entries = match fs::read_dir(inbox_dir) {
        Ok(rd) => rd,
        Err(_) => return out, // no global inbox yet: nothing to do
    };

    let mut files: Vec<PathBuf> = entries
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| p.is_file() && p.extension().and_then(|e| e.to_str()) == Some("json"))
        .collect();
    files.sort();

    for path in files.into_iter().take(max_files) {
        let name = path
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("unnamed.json")
            .to_string();

        // An unreadable file has an undeterminable target (Unknown) -> claimable-to-reject.
        let text = fs::read_to_string(&path).ok();
        let target = match &text {
            Some(t) => peek_target(t),
            None => Target::Unknown,
        };
        if owns(&target) == Claim::Leave {
            continue; // addressed to a project another instance owns: leave in place
        }

        // Claimable. Re-parse (full validation) to decide processed vs rejected, then let
        // the atomic rename be the actual claim.
        let parsed = match &text {
            Some(t) => parse(t, &name),
            None => Err(format!("could not read file: {name}")),
        };
        match parsed {
            Ok((command, ack)) => {
                if claim_move(inbox_dir, &path, PROCESSED_DIR, &name) {
                    out.push(InboxOutcome::Accepted { file: name, command, ack });
                }
                // else: lost the claim race -> skip silently
            }
            Err(reason) => {
                if claim_move(inbox_dir, &path, REJECTED_DIR, &name) {
                    write_error_note(inbox_dir, &name, &reason);
                    out.push(InboxOutcome::Rejected { file: name, reason });
                }
                // else: lost the claim race -> skip silently
            }
        }
    }

    out
}

/// Atomic claim: ensure `sub/` exists, then rename the file into it. Returns whether THIS
/// call won (the rename succeeded). A failure means a racing instance already claimed the
/// file (its source is gone) or the destination slot is unusable — either way the caller
/// skips it silently. Distinct from [`move_to`] (the workspace path's best-effort move,
/// which must stay byte-for-byte unchanged): here the rename result IS the claim signal.
fn claim_move(inbox_dir: &Path, path: &Path, sub: &str, name: &str) -> bool {
    let dest_dir = inbox_dir.join(sub);
    if fs::create_dir_all(&dest_dir).is_err() {
        return false;
    }
    fs::rename(path, dest_dir.join(name)).is_ok()
}

#[cfg(test)]
#[path = "inbox_test.rs"]
mod inbox_test;
