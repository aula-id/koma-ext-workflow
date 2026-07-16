//! Size-capped digest builders: the `context.set` board blob (ARCHITECTURE.md 6.6)
//! and the panel snapshot JSON (ARCHITECTURE.md 10.2), full and summary modes.

use crate::domain::{ChatAuthor, Column, Comment, CommentAuthor, ParkReason, Project, ProjectPhase, Receipt, Sprint, SprintStatus, Task, TaskState};
use serde_json::{json, Value};
use std::collections::HashMap;

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

/// The wire string for a sprint status (feature: sprints).
fn sprint_status_str(s: &SprintStatus) -> &'static str {
    match s {
        SprintStatus::Pending => "pending",
        SprintStatus::Active => "active",
        SprintStatus::InReview => "inreview",
        SprintStatus::Done => "done",
    }
}

/// How many of a sprint's tasks are Done (feature: sprints) — the `n` in the `n/m` progress.
fn sprint_done_count(p: &Project, sprint: &Sprint) -> usize {
    sprint
        .tasks
        .iter()
        .filter(|tid| {
            p.tasks
                .iter()
                .any(|t| &t.id == *tid && matches!(t.state, TaskState::Done { .. }))
        })
        .count()
}

/// The index of the project's CURRENT sprint (feature: sprints): the `Active` one, else the
/// `InReview` one (ceremony in flight). `None` for the legacy no-sprint flow or once all are Done.
fn current_sprint_idx(p: &Project) -> Option<usize> {
    p.sprints
        .iter()
        .position(|s| matches!(s.status, SprintStatus::Active))
        .or_else(|| p.sprints.iter().position(|s| matches!(s.status, SprintStatus::InReview)))
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
                ParkReason::AuditFailed(r) => {
                    format!("audit failed: {}", truncate_bytes(r, CAP_ATTENTION_REASON))
                }
                ParkReason::InstantDeath(r) => {
                    format!("chronic instant death: {}", truncate_bytes(r, CAP_ATTENTION_REASON))
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
        "- {}: phase={} track={} done {}/{} running={} parked={} delivery={}\n",
        p.id.0,
        phase_str(&p.phase),
        p.track,
        done,
        total,
        running,
        parked,
        delivery
    );

    // Sprints (feature: sprints): a compact current-sprint line (index/count, goal, n/m tasks, and
    // whether the review is in flight). Omitted for the legacy no-sprint flow.
    if let Some(i) = current_sprint_idx(p) {
        let s = &p.sprints[i];
        let done = sprint_done_count(p, s);
        let review = if matches!(s.status, SprintStatus::InReview) { " (in review)" } else { "" };
        line.push_str(&format!(
            "  sprint {}/{}: {} ({}/{} tasks){}\n",
            i + 1,
            p.sprints.len(),
            truncate_bytes(s.goal.trim(), 80),
            done,
            s.tasks.len(),
            review
        ));
    }

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

/// The office brain's current live activity for a project (e.g. "drafting the TRD"), plus
/// when it started — surfaced on the panel snapshot (full mode only) and used to drive an
/// elapsed-time display. See the driver's `office_activity` for how this is derived.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OfficeActivity {
    pub label: String,
    pub since_ms: u64,
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

/// The short worker persona (e.g. `nova`) at a task's desk, for the pixel office view
/// (ARCHITECTURE.md 5.2, ui/src/views/OfficeMap.tsx). Present only while the task is
/// occupying a worker's desk — in progress, in review, or parked mid-work — since those
/// are the states the office map draws a seated persona for; Todo/Backlog/Done free the
/// desk. The value is the stable id-hashed worker persona, identical to the task's
/// `office-worker-` binding persona with its prefix stripped (a reviewer binding carries
/// `office-reviewer`, so it is re-derived from the id rather than stripped). Full mode
/// only; summary mode drops it under the 900KB size guard.
fn task_persona(t: &Task) -> Option<&'static str> {
    let occupied = matches!(
        t.state,
        TaskState::OnProgress { .. } | TaskState::Review { .. } | TaskState::Parked { .. }
    );
    occupied.then(|| crate::persona::worker_persona(&t.id.0))
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
        // Office-view desk label (5.2); omitted entirely when the desk is free.
        if let Some(name) = task_persona(t) {
            obj["persona"] = json!(name);
        }
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

// ---------------------------------------------------------------------------
// Design-stage cards (feature: design-stage-cards) — while a project is pre-Ready (Drafting, or
// paused mid-Drafting via Interrupted), its kanban board has no real task cards yet (those only
// exist once a breakdown lands, Drafting -> Ready). Rather than render an empty board while all
// the SDLC action (triage -> PRD/change-brief -> research -> TRD+CRD -> breakdown) hides in the
// trace tab, the panel snapshot carries a parallel `designStages` array: lightweight placeholder
// cards the UI renders in the kanban columns by status, derived entirely from existing kernel
// state (see `Project::research_skip_reason` for the one durable field this feature added, since
// the "why was research skipped" fact is not otherwise recoverable without parsing the capped
// trace ring).
// ---------------------------------------------------------------------------

/// One design-stage placeholder card. `status` is one of `"todo" | "inProgress" | "done"`.
struct DesignStage {
    id: &'static str,
    label: &'static str,
    status: &'static str,
    note: Option<String>,
}

fn design_stage_value(s: DesignStage) -> Value {
    let mut v = json!({ "id": s.id, "label": s.label, "status": s.status });
    if let Some(note) = s.note {
        v["note"] = json!(note);
    }
    v
}

fn todo_stage(id: &'static str, label: &'static str) -> DesignStage {
    DesignStage { id, label, status: "todo", note: None }
}

/// Whether `p` is still pre-Ready for the design-stage board's purposes: actively Drafting, or
/// paused mid-Drafting (`Interrupted` with `interrupted_from == Some(Drafting)` — feature:
/// interrupt-from-drafting). Every other phase (`Ready`/`Running`/`Halted`/`Done`, or an
/// `Interrupted` that came from one of those) means a real board exists.
fn is_pre_ready_drafting(p: &Project) -> bool {
    matches!(p.phase, ProjectPhase::Drafting)
        || (matches!(p.phase, ProjectPhase::Interrupted)
            && matches!(p.interrupted_from, Some(ProjectPhase::Drafting)))
}

/// The Triage stage: `inProgress` while the classifier invoke is in flight (`triage_pending`);
/// `done` once the track is resolved, noting which track it landed on.
fn triage_stage(p: &Project) -> DesignStage {
    if p.triage_pending {
        DesignStage { id: "triage", label: "Triage", status: "inProgress", note: None }
    } else {
        DesignStage { id: "triage", label: "Triage", status: "done", note: Some(p.track.clone()) }
    }
}

/// Whether the newest captured doc-set is TRD+CRD rather than PRD/change-brief (mirrors the
/// kernel's own `newest_gated_doc` in kernel.rs — the pipeline authors PRD then TRD+CRD strictly
/// in order, so once either is non-empty the PRD-stage gate has necessarily already cleared).
fn newest_doc_is_trdcrd(p: &Project) -> bool {
    !p.trd_markdown.trim().is_empty() || !p.crd_markdown.trim().is_empty()
}

/// The PRD (or, on the enhancement track, "Change brief") stage: `inProgress` while the doc is
/// still empty, or captured but its safeguard gate has not yet cleared; `done` once the gate has
/// cleared (verified once TRD+CRD — or, for enhancement, the breakdown — has moved on, since the
/// shared `gate_cleared` flag has by then been reset for the NEXT join). Returns whether the
/// stage is done, so callers gating on "PRD/change-brief settled" (the TRD+CRD stage) don't have
/// to re-derive it.
fn prd_stage(p: &Project) -> (DesignStage, bool) {
    let label = if p.track == "enhancement" { "Change brief" } else { "PRD" };
    if p.prd_markdown.trim().is_empty() {
        return (DesignStage { id: "prd", label, status: "inProgress", note: None }, false);
    }
    if newest_doc_is_trdcrd(p) || p.gate_cleared {
        (
            DesignStage { id: "prd", label, status: "done", note: Some("verified — clean".to_string()) },
            true,
        )
    } else {
        (DesignStage { id: "prd", label, status: "inProgress", note: None }, false)
    }
}

/// Friendly note text for a settled `research_skip_reason` (see the field's doc comment in
/// domain.rs for the kernel sites that set each value).
fn research_skip_note(reason: &str) -> String {
    match reason {
        "config" => "skipped (config)".to_string(),
        "well-known" => "skipped — stack well-known".to_string(),
        "user" => "skipped — by user".to_string(),
        "degraded" => "skipped — research degraded".to_string(),
        other => format!("skipped ({other})"),
    }
}

/// The Research stage: `todo` before the PRD/change-brief that triggers it is even captured;
/// `inProgress` while the `office-researcher` binding is live; `done` once notes land, or once a
/// skip/degrade is recorded (`research_skip_reason` — see domain.rs; a durable field rather than
/// parsed trace, since the trace ring is capped and could evict the entry).
fn research_stage(p: &Project) -> DesignStage {
    if p.prd_markdown.trim().is_empty() {
        return todo_stage("research", "Research");
    }
    if p.research.is_some() {
        return DesignStage { id: "research", label: "Research", status: "inProgress", note: None };
    }
    if !p.research_notes.trim().is_empty() {
        return DesignStage {
            id: "research",
            label: "Research",
            status: "done",
            note: Some("researched".to_string()),
        };
    }
    if let Some(reason) = &p.research_skip_reason {
        return DesignStage {
            id: "research",
            label: "Research",
            status: "done",
            note: Some(research_skip_note(reason)),
        };
    }
    // Captured but the "always spawn now" vs "auto: defer to the gate's well-known answer" vs
    // "never" decision hasn't landed yet (a same-tick window) — treat as in flight rather than
    // flashing back to todo.
    DesignStage { id: "research", label: "Research", status: "inProgress", note: None }
}

/// The TRD+CRD stage (project track only — enhancement skips the trio entirely, see
/// `enhancement_track_stages`): `todo` until the PRD gate clears AND research settles (the exact
/// join condition `maybe_author_trdcrd` fires on, kernel.rs); `inProgress` once that join is met
/// but the invoke hasn't returned both docs; `done` once both are captured.
fn trdcrd_stage(p: &Project, prd_done: bool) -> DesignStage {
    let trd_done = !p.trd_markdown.trim().is_empty();
    let crd_done = !p.crd_markdown.trim().is_empty();
    if trd_done && crd_done {
        DesignStage { id: "trdcrd", label: "TRD+CRD", status: "done", note: None }
    } else if trd_done || crd_done || (prd_done && p.research.is_none()) {
        DesignStage { id: "trdcrd", label: "TRD+CRD", status: "inProgress", note: None }
    } else {
        DesignStage { id: "trdcrd", label: "TRD+CRD", status: "todo", note: None }
    }
}

/// The Breakdown stage (every track ends here): `done` once the board is built (`p.tasks` non-
/// empty); `inProgress` while a validated breakdown is stashed awaiting its gate
/// (`pending_breakdown`, design-speedup item 8); `todo` otherwise.
fn breakdown_stage(p: &Project) -> DesignStage {
    if !p.tasks.is_empty() {
        DesignStage { id: "breakdown", label: "Breakdown", status: "done", note: None }
    } else if p.pending_breakdown.is_some() {
        DesignStage {
            id: "breakdown",
            label: "Breakdown",
            status: "inProgress",
            note: Some("planned — awaiting gate".to_string()),
        }
    } else {
        todo_stage("breakdown", "Breakdown")
    }
}

/// The single Task card for the patch track (feature: sdlc-triage — patch skips every document
/// and goes straight to one task): `todo`/`done` mirror the task's own state; `inProgress` once
/// it's occupying a worker's desk (dispatched — `OnProgress`/`Review`/`Parked`, the same
/// "occupied" test `task_persona` uses).
fn patch_task_stage(p: &Project) -> DesignStage {
    match p.tasks.first() {
        None => todo_stage("task", "Task"),
        Some(t) => {
            let status = match &t.state {
                TaskState::Done { .. } => "done",
                TaskState::OnProgress { .. } | TaskState::Review { .. } | TaskState::Parked { .. } => {
                    "inProgress"
                }
                _ => "todo",
            };
            DesignStage { id: "task", label: "Task", status, note: None }
        }
    }
}

/// Build the `designStages` array for a pre-Ready project, or `None` once it has a real board
/// (`Ready`+). Only [`triage`] is shown while the classifier invoke is still resolving the track
/// (`triage_pending`) — the rest render as `todo` placeholders under the safe-fallback "project"
/// shape, since which track actually applies is not known yet.
fn design_stages(p: &Project) -> Option<Vec<Value>> {
    if !is_pre_ready_drafting(p) {
        return None;
    }

    let mut stages = vec![triage_stage(p)];

    if p.triage_pending {
        stages.push(todo_stage("prd", "PRD"));
        stages.push(todo_stage("research", "Research"));
        stages.push(todo_stage("trdcrd", "TRD+CRD"));
        stages.push(todo_stage("breakdown", "Breakdown"));
        return Some(stages.into_iter().map(design_stage_value).collect());
    }

    match p.track.as_str() {
        "patch" => stages.push(patch_task_stage(p)),
        "enhancement" => {
            let (prd, _prd_done) = prd_stage(p);
            stages.push(prd);
            stages.push(research_stage(p));
            stages.push(breakdown_stage(p));
        }
        _ => {
            // "project" — the default and safe fallback track.
            let (prd, prd_done) = prd_stage(p);
            stages.push(prd);
            stages.push(research_stage(p));
            stages.push(trdcrd_stage(p, prd_done));
            stages.push(breakdown_stage(p));
        }
    }

    Some(stages.into_iter().map(design_stage_value).collect())
}

fn project_to_value(p: &Project, mode: SnapshotMode, activity: Option<&OfficeActivity>) -> Value {
    let tasks: Vec<Value> = p.tasks.iter().map(|t| task_to_value(t, mode)).collect();

    let mut obj = json!({
        "id": p.id.0,
        "name": p.name,
        "phase": phase_value(&p.phase),
        // SDLC intake track (feature: sdlc-triage): project | enhancement | patch. In the base
        // object (both modes) so the panel can always badge the track.
        "track": p.track,
        "deliveryPath": p.delivery_path.as_ref().map(|pb| pb.display().to_string()),
        "seq": p.seq,
        "tasks": tasks,
    });

    if mode == SnapshotMode::Full {
        obj["prdMarkdown"] = json!(p.prd_markdown);
        // The TRD + research notes (6.2b) + CRD (6.2c) ride full mode exactly like prdMarkdown;
        // summary mode drops them all under the 900KB size guard.
        obj["trdMarkdown"] = json!(p.trd_markdown);
        obj["researchNotes"] = json!(p.research_notes);
        obj["crdMarkdown"] = json!(p.crd_markdown);
        // The last clean-build audit grade (6.2c) — surfaced on the dashboard row + MCP status
        // line when present (null when the project was never audited).
        obj["lastAuditGrade"] = json!(p.last_audit_grade);
        // Fixed-staff liveness for the office view (5.2): whether the project-level
        // researcher / clean-build auditor sub-agent is currently in flight. The office map
        // animates the researcher reading / the auditor judging off these; additive, full
        // mode only.
        obj["researchActive"] = json!(p.research.is_some());
        obj["auditActive"] = json!(p.audit.is_some());
        // The office brain's current live activity (e.g. "drafting the TRD") with a start
        // timestamp, for an elapsed-time display; additive, full mode only, omitted (not
        // null) when nothing is currently live.
        if let Some(a) = activity {
            obj["officeActivity"] = json!({ "label": a.label, "sinceMs": a.since_ms });
        }
        // Ungrounded assumptions the safeguard flagged in the last doc gate (6.2c): the docs tab
        // renders these as an amber pending-assumptions strip while the pipeline waits.
        obj["pendingAssumptions"] = json!(p.pending_assumptions);
        // Design-stage placeholder cards (feature: design-stage-cards): while the project is
        // pre-Ready (Drafting, or paused mid-Drafting), the panel renders these in the kanban
        // columns instead of an empty board. Absent once a real board exists (Ready+).
        if let Some(stages) = design_stages(p) {
            obj["designStages"] = json!(stages);
        }
        // Machine-diary trace ring (feature: tracelog): the panel's trace tab renders these as an
        // `HH:MM:SS kind summary` timeline. Full mode only, like the other bodies; summary mode
        // drops it under the 900KB size guard.
        obj["trace"] = json!(p
            .trace
            .iter()
            .map(|e| json!({ "ts": e.ts, "kind": e.kind, "summary": e.summary }))
            .collect::<Vec<_>>());
        obj["officeTranscript"] = json!(p
            .office_transcript
            .iter()
            .map(|m| json!({ "who": chat_author_str(&m.who), "text": m.text }))
            .collect::<Vec<_>>());
        obj["officeSummary"] = json!(p.office_summary);
        // The panel's `config_set` form (Settings.tsx) needs to read back what it last
        // saved (10.2 round-trip); only the fields that form edits (not `officeRole`/
        // `workerMaxRuntimeMs`/`safeguardRole`, which have no panel affordance yet).
        obj["config"] = json!({
            "maxWorkers": p.config.max_workers,
            "bounceBudget": p.config.bounce_budget,
            "workerModel": p.config.worker_model,
            "reviewerModel": p.config.reviewer_model,
            "keepDesks": p.config.keep_desks,
            "crdPassGrade": p.config.crd_pass_grade,
            "assumptionCheck": p.config.assumption_check,
            "assumptionMode": p.config.assumption_mode,
            // design-speedup item 4: the research policy + optional doc-drafting model override,
            // surfaced so the Settings panel can read back what it last saved (10.2 round-trip).
            "researchMode": p.config.research_mode,
            "drafterModel": p.config.drafter_model,
        });
        // Sprints (feature: sprints): the full sprint list with statuses + n/m progress, and a
        // pointer to the CURRENT sprint (index/goal/progress + whether its review is in flight).
        // During a review the reviewed sprint carries its ceremony transcript so the UI can replay
        // it as chat bubbles. Empty/absent for the legacy no-sprint flow. Full mode only.
        if !p.sprints.is_empty() {
            obj["sprints"] = json!(p
                .sprints
                .iter()
                .enumerate()
                .map(|(i, s)| {
                    let mut so = json!({
                        "index": i,
                        "goal": s.goal,
                        "status": sprint_status_str(&s.status),
                        "total": s.tasks.len(),
                        "done": sprint_done_count(p, s),
                        "tasks": s.tasks.iter().map(|t| t.0.clone()).collect::<Vec<_>>(),
                    });
                    if matches!(s.status, SprintStatus::InReview) {
                        so["transcript"] = json!(s
                            .transcript
                            .iter()
                            .map(|l| json!({ "speaker": l.speaker, "text": l.line }))
                            .collect::<Vec<_>>());
                    }
                    so
                })
                .collect::<Vec<_>>());
            if let Some(i) = current_sprint_idx(p) {
                let s = &p.sprints[i];
                obj["activeSprint"] = json!({
                    "index": i,
                    "count": p.sprints.len(),
                    "goal": s.goal,
                    "total": s.tasks.len(),
                    "done": sprint_done_count(p, s),
                    "inReview": matches!(s.status, SprintStatus::InReview),
                });
            }
        }
    }

    obj
}

/// Build the panel snapshot payload for `projects` in the given mode. Returns a
/// JSON array — the driver (W7) wraps it in the frozen envelope
/// `{ kind: "snapshot", seq, projects: [...] }` and applies the 900KB size guard.
pub fn panel_snapshot(projects: &[Project], mode: SnapshotMode) -> Value {
    panel_snapshot_with_activity(projects, mode, None)
}

/// Like [`panel_snapshot`], but additionally threads in each project's live "office
/// activity" (keyed by project id), when the caller has one to report.
pub fn panel_snapshot_with_activity(
    projects: &[Project],
    mode: SnapshotMode,
    activity: Option<&HashMap<String, OfficeActivity>>,
) -> Value {
    Value::Array(
        projects
            .iter()
            .map(|p| project_to_value(p, mode, activity.and_then(|m| m.get(&p.id.0))))
            .collect(),
    )
}
