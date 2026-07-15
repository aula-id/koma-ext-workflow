//! The read-only status digest for the `workflow_status` tool.
//!
//! Unlike the command tools, `workflow_status` does not go through the inbox at all: it
//! reads the durable store DIRECTLY and renders a plain-text summary. It is careful to
//! NEVER write: it does not create the state root when it is absent, and it reads the
//! registry + each project's `state.json` individually rather than calling
//! `Store::load_all` (which heals and rewrites the registry, and quarantines corrupt dirs).

use std::path::Path;

use office_core::{column, Column, ParkReason, Project, ProjectPhase, TaskState};
use office_store::Store;

/// Build the status digest for the default store root (`office_store::root()`), scoped to
/// one project when `focus` is `Some`.
pub fn status_digest(focus: Option<&str>) -> String {
    status_digest_at(&office_store::root(), focus)
}

/// Build the status digest for a specific root. Read-only: if the root does not exist there
/// is nothing to report (and we must not create it); otherwise open the store and read the
/// registry + each project's state, WITHOUT the healing rewrite `load_all` would perform.
pub fn status_digest_at(root: &Path, focus: Option<&str>) -> String {
    if !root.exists() {
        return format!(
            "No Workflow projects yet (state root {} does not exist).",
            root.display()
        );
    }
    let store = match Store::open(root) {
        Ok(s) => s,
        Err(e) => return format!("workflow: cannot open state root {}: {e}", root.display()),
    };
    let rows = store.registry().unwrap_or_default();
    let mut projects: Vec<Project> = Vec::new();
    for row in &rows {
        // Skip any project whose state fails to load rather than mutating the store.
        if let Ok(p) = store.load_project(&row.project_id) {
            projects.push(p);
        }
    }
    render(&projects, focus)
}

/// Render a digest for the loaded projects. Pure — the store-seeded tests exercise this via
/// [`status_digest_at`], and it is factored out so rendering is independent of IO.
pub fn render(projects: &[Project], focus: Option<&str>) -> String {
    match focus {
        Some(id) => match projects.iter().find(|p| p.id.0 == id) {
            Some(p) => render_detail(p),
            None => {
                let known: Vec<&str> = projects.iter().map(|p| p.id.0.as_str()).collect();
                if known.is_empty() {
                    format!("Project '{id}' not found. No Workflow projects exist yet.")
                } else {
                    format!("Project '{id}' not found. Known projects: {}.", known.join(", "))
                }
            }
        },
        None => render_all(projects),
    }
}

fn render_all(projects: &[Project]) -> String {
    if projects.is_empty() {
        return "No Workflow projects yet.".to_string();
    }
    let mut out = format!("Workflow: {} project(s)\n", projects.len());
    // Most-recently-active first (by seq), matching the context blob ordering.
    let mut ordered: Vec<&Project> = projects.iter().collect();
    ordered.sort_by(|a, b| b.seq.cmp(&a.seq).then_with(|| a.id.0.cmp(&b.id.0)));
    for p in ordered {
        out.push('\n');
        out.push_str(&project_block(p));
    }
    out
}

/// The per-project summary block: id/name/phase, column counts, parked reasons, total
/// bounces, and pending (unsent) outbox notices.
fn project_block(p: &Project) -> String {
    let c = column_counts(p);
    let mut s = format!("{} ({}) - {}\n", p.id.0, p.name, phase_label(&p.phase));
    s.push_str(&format!(
        "  columns: backlog {} todo {} onprogress {} review {} done {}\n",
        c[0], c[1], c[2], c[3], c[4]
    ));
    let parked = parked_lines(p);
    if parked.is_empty() {
        s.push_str("  parked: none\n");
    } else {
        s.push_str(&format!("  parked: {}\n", parked.join("; ")));
    }
    let bounces: u32 = p.tasks.iter().map(|t| t.bounces).sum();
    s.push_str(&format!("  bounces: {bounces}\n"));
    let pending = p.outbox.iter().filter(|n| !n.sent).count();
    s.push_str(&format!("  outbox: {pending} pending\n"));
    // Drafting-pipeline docs presence (ARCHITECTURE.md 6.2b/6.2c): PRD -> research -> TRD -> CRD.
    s.push_str(&format!(
        "  docs: prd {}, trd {}, research {}, crd {}\n",
        yn(!p.prd_markdown.trim().is_empty()),
        yn(!p.trd_markdown.trim().is_empty()),
        yn(!p.research_notes.trim().is_empty()),
        yn(!p.crd_markdown.trim().is_empty()),
    ));
    // The last clean-build audit grade (6.2c), only when the project has been audited.
    if let Some(g) = p.last_audit_grade {
        s.push_str(&format!("  audit: {g}\n"));
    }
    s
}

fn yn(present: bool) -> &'static str {
    if present {
        "yes"
    } else {
        "no"
    }
}

/// Single-project detail: the summary block plus the delivery path and a per-task listing.
fn render_detail(p: &Project) -> String {
    let mut s = project_block(p);
    let delivery = p
        .delivery_path
        .as_ref()
        .map(|pb| pb.display().to_string())
        .unwrap_or_else(|| "none".to_string());
    s.push_str(&format!("  delivery: {delivery}\n"));
    if p.tasks.is_empty() {
        s.push_str("  tasks: none\n");
    } else {
        s.push_str("  tasks:\n");
        for t in &p.tasks {
            s.push_str(&format!(
                "    {} [{}] {} (bounces {})\n",
                t.id.0,
                column_label(column(&t.state)),
                t.title,
                t.bounces
            ));
        }
    }
    s
}

fn column_counts(p: &Project) -> [usize; 5] {
    let mut c = [0usize; 5];
    for t in &p.tasks {
        let idx = match column(&t.state) {
            Column::Backlog => 0,
            Column::Todo => 1,
            Column::OnProgress => 2,
            Column::Review => 3,
            Column::Done => 4,
        };
        c[idx] += 1;
    }
    c
}

fn column_label(c: Column) -> &'static str {
    match c {
        Column::Backlog => "backlog",
        Column::Todo => "todo",
        Column::OnProgress => "onprogress",
        Column::Review => "review",
        Column::Done => "done",
    }
}

fn parked_lines(p: &Project) -> Vec<String> {
    p.tasks
        .iter()
        .filter_map(|t| match &t.state {
            TaskState::Parked { reason, .. } => {
                Some(format!("{} ({})", t.id.0, park_reason_label(reason)))
            }
            _ => None,
        })
        .collect()
}

fn park_reason_label(reason: &ParkReason) -> String {
    match reason {
        ParkReason::ReviewBounceBudget => "bounce budget exceeded".to_string(),
        ParkReason::WorkerBlocked(r) => format!("worker blocked: {r}"),
        ParkReason::SpawnFailed(r) => format!("spawn failed: {r}"),
        ParkReason::AuditFailed(r) => format!("audit failed: {r}"),
    }
}

fn phase_label(phase: &ProjectPhase) -> String {
    match phase {
        ProjectPhase::Drafting => "drafting".to_string(),
        ProjectPhase::Ready => "ready".to_string(),
        ProjectPhase::Running => "running".to_string(),
        ProjectPhase::Interrupted => "interrupted".to_string(),
        ProjectPhase::Halted { reason } => format!("halted: {reason}"),
        ProjectPhase::Done { .. } => "done".to_string(),
    }
}

#[cfg(test)]
#[path = "status_test.rs"]
mod status_test;
