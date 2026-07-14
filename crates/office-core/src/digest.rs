//! Size-capped digest builders: the `context.set` board blob (ARCHITECTURE.md 6.6)
//! and the panel snapshot JSON (ARCHITECTURE.md 10.2), full and summary modes.

use crate::domain::{ChatAuthor, Column, Comment, CommentAuthor, ParkReason, Project, ProjectPhase, Receipt, Task, TaskState};
use serde_json::{json, Value};

/// Host cap for `context.set` is 8192 bytes, boundary-inclusive
/// (broker.rs:2784); we self-limit to 7900 to leave headroom.
pub const CONTEXT_BLOB_CAP: usize = 7900;

const CAP_DELIVERY_PATH: usize = 200;
const CAP_ATTENTION_REASON: usize = 120;

fn truncate_bytes(s: &str, max: usize) -> String {
    if s.len() <= max {
        return s.to_string();
    }
    const MARKER: &str = "...";
    let budget = max.saturating_sub(MARKER.len());
    let mut end = budget.min(s.len());
    while end > 0 && !s.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}{}", &s[..end], MARKER)
}

fn phase_str(phase: &ProjectPhase) -> &'static str {
    match phase {
        ProjectPhase::Drafting => "drafting",
        ProjectPhase::Ready => "ready",
        ProjectPhase::Running => "running",
        ProjectPhase::Interrupted => "interrupted",
        ProjectPhase::Halted { .. } => "halted",
        ProjectPhase::Done { .. } => "done",
    }
}

fn is_running_state(state: &TaskState) -> bool {
    matches!(state, TaskState::OnProgress { .. } | TaskState::Review { .. })
}

/// Up to two "halt/park" one-liners for a project's `attention:` line: the halt
/// reason first (if halted), then parked-task reasons, in task-id order.
fn attention_lines(p: &Project) -> Vec<String> {
    let mut out = Vec::new();
    if let ProjectPhase::Halted { reason } = &p.phase {
        out.push(format!("halted: {}", truncate_bytes(reason, CAP_ATTENTION_REASON)));
    }
    for t in &p.tasks {
        if out.len() >= 2 {
            break;
        }
        if let TaskState::Parked { reason, .. } = &t.state {
            let label = match reason {
                ParkReason::ReviewBounceBudget => "bounce budget exceeded".to_string(),
                ParkReason::WorkerBlocked(r) => {
                    format!("worker blocked: {}", truncate_bytes(r, CAP_ATTENTION_REASON))
                }
                ParkReason::SpawnFailed(r) => {
                    format!("spawn failed: {}", truncate_bytes(r, CAP_ATTENTION_REASON))
                }
            };
            out.push(format!("{} parked: {}", t.id.0, label));
        }
    }
    out.truncate(2);
    out
}

fn project_line(p: &Project) -> String {
    let total = p.tasks.len();
    let done = p.tasks.iter().filter(|t| matches!(t.state, TaskState::Done { .. })).count();
    let running = p.tasks.iter().filter(|t| is_running_state(&t.state)).count();
    let parked = p.tasks.iter().filter(|t| matches!(t.state, TaskState::Parked { .. })).count();
    let delivery = p
        .delivery_path
        .as_ref()
        .map(|pb| truncate_bytes(&pb.display().to_string(), CAP_DELIVERY_PATH))
        .unwrap_or_else(|| "none".to_string());

    let mut line = format!(
        "- {}: phase={} done {}/{} running={} parked={} delivery={}\n",
        p.id.0,
        phase_str(&p.phase),
        done,
        total,
        running,
        parked,
        delivery
    );

    let attn = attention_lines(p);
    if !attn.is_empty() {
        line.push_str(&format!("  attention: {}\n", attn.join("; ")));
    }
    line
}

/// Build the `context.set` board digest (ARCHITECTURE.md 6.6). Always fits in
/// `CONTEXT_BLOB_CAP` bytes: the instruction block (header + inbox-reach footer)
/// is never truncated; projects are listed most-recently-active first (by `seq`,
/// the only recency signal the domain model carries — bumped on every mutation)
/// and dropped tail-first once the remaining budget can't fit the next one.
pub fn context_blob(projects: &[Project]) -> String {
    let header = format!(
        "# Workflow\nActive projects: {}. Panel: Workflow tab.\n",
        projects.len()
    );
    let footer = "To reach the office from chat, write koma-workflow/inbox/<millis>-<slug>.json:\n\
{\"op\":\"brief\",\"project\":\"<id>\",\"message\":\"...\"} (ops: brief,status,authorize,interrupt,resume,comment)\n";

    let mut ordered: Vec<&Project> = projects.iter().collect();
    ordered.sort_by(|a, b| b.seq.cmp(&a.seq).then_with(|| a.id.cmp(&b.id)));

    let instruction_len = header.len() + footer.len();
    let mut budget = CONTEXT_BLOB_CAP.saturating_sub(instruction_len);

    let mut body = String::new();
    for p in ordered {
        let line = project_line(p);
        if line.len() > budget {
            break;
        }
        budget -= line.len();
        body.push_str(&line);
    }

    format!("{header}{body}{footer}")
}

/// Full vs summary rendering for `panel_snapshot`. The size guard (900KB total,
/// promote-to-summary-on-overflow) lives in the driver (W7); this module only
/// supports both shapes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SnapshotMode {
    Full,
    Summary,
}

fn column_str(c: Column) -> &'static str {
    match c {
        Column::Backlog => "backlog",
        Column::Todo => "todo",
        Column::OnProgress => "onprogress",
        Column::Review => "review",
        Column::Done => "done",
    }
}

fn state_label(state: &TaskState) -> &'static str {
    match state {
        TaskState::Backlog => "backlog",
        TaskState::Todo => "todo",
        TaskState::OnProgress { .. } => "onprogress",
        TaskState::Review { .. } => "review",
        TaskState::Parked { .. } => "parked",
        TaskState::Done { .. } => "done",
    }
}

fn chat_author_str(who: &ChatAuthor) -> &'static str {
    match who {
        ChatAuthor::User => "user",
        ChatAuthor::Office => "office",
    }
}

fn comment_author_str(who: &CommentAuthor) -> &'static str {
    match who {
        CommentAuthor::User => "user",
        CommentAuthor::Office => "office",
        CommentAuthor::System => "system",
    }
}

fn receipt_value(r: &Receipt) -> Value {
    match r {
        Receipt::Pending => json!({ "state": "pending" }),
        Receipt::Delivered { at_ms } => json!({ "state": "delivered", "atMs": at_ms }),
        Receipt::Read { at_ms } => json!({ "state": "read", "atMs": at_ms }),
    }
}

fn comment_to_value(c: &Comment) -> Value {
    json!({
        "id": c.id.0,
        "author": comment_author_str(&c.author),
        "text": c.text,
        "createdMs": c.created_ms,
        "receipt": receipt_value(&c.receipt),
    })
}

fn task_to_value(t: &Task, mode: SnapshotMode) -> Value {
    let mut obj = json!({
        "id": t.id.0,
        "title": t.title,
        "column": column_str(crate::domain::column(&t.state)),
        "state": state_label(&t.state),
        "priority": t.priority,
        "blockedBy": t.blocked_by.iter().map(|id| id.0.clone()).collect::<Vec<_>>(),
        "bounces": t.bounces,
    });

    // Full mode carries the report/history bodies the task detail view renders;
    // summary mode keeps only counts/states (ARCHITECTURE.md 10.2 size guard).
    if mode == SnapshotMode::Full {
        obj["description"] = json!(t.description);
        obj["acceptance"] = json!(t.acceptance);
        obj["comments"] = json!(t.comments.iter().map(comment_to_value).collect::<Vec<_>>());
        obj["lastReport"] = json!(t.last_report);
        obj["lastReview"] = json!(t.last_review);
        obj["history"] = json!(t
            .history
            .iter()
            .map(|e| json!({ "atMs": e.at_ms, "event": e.event }))
            .collect::<Vec<_>>());
    }

    obj
}

fn phase_value(phase: &ProjectPhase) -> Value {
    match phase {
        ProjectPhase::Halted { reason } => json!({ "kind": "halted", "reason": reason }),
        ProjectPhase::Done { at_ms } => json!({ "kind": "done", "atMs": at_ms }),
        other => json!({ "kind": phase_str(other) }),
    }
}

fn project_to_value(p: &Project, mode: SnapshotMode) -> Value {
    let tasks: Vec<Value> = p.tasks.iter().map(|t| task_to_value(t, mode)).collect();

    let mut obj = json!({
        "id": p.id.0,
        "name": p.name,
        "phase": phase_value(&p.phase),
        "deliveryPath": p.delivery_path.as_ref().map(|pb| pb.display().to_string()),
        "seq": p.seq,
        "tasks": tasks,
    });

    if mode == SnapshotMode::Full {
        obj["prdMarkdown"] = json!(p.prd_markdown);
        obj["officeTranscript"] = json!(p
            .office_transcript
            .iter()
            .map(|m| json!({ "who": chat_author_str(&m.who), "text": m.text }))
            .collect::<Vec<_>>());
        obj["officeSummary"] = json!(p.office_summary);
        // The panel's `config_set` form (Settings.tsx) needs to read back what it last
        // saved (10.2 round-trip); only the fields that form edits (not `officeRole`/
        // `workerMaxRuntimeMs`, which have no panel affordance yet).
        obj["config"] = json!({
            "maxWorkers": p.config.max_workers,
            "bounceBudget": p.config.bounce_budget,
            "workerModel": p.config.worker_model,
            "reviewerModel": p.config.reviewer_model,
            "keepDesks": p.config.keep_desks,
        });
    }

    obj
}

/// Build the panel snapshot payload for `projects` in the given mode. Returns a
/// JSON array — the driver (W7) wraps it in the frozen envelope
/// `{ kind: "snapshot", seq, projects: [...] }` and applies the 900KB size guard.
pub fn panel_snapshot(projects: &[Project], mode: SnapshotMode) -> Value {
    Value::Array(projects.iter().map(|p| project_to_value(p, mode)).collect())
}
