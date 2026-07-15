//! Worker/reviewer spawn prompt assembly and the front-office persona head.
//!
//! Every builder here is a pure string function: same inputs -> same output. The
//! variable, LLM-authored sections (description, review notes, comment text,
//! acceptance criteria) are byte-capped BEFORE assembly so the whole prompt stays
//! comfortably bounded even when fed pathological (oversized) input — see
//! ARCHITECTURE.md 8.

use crate::domain::{Comment, Epic, Project, Story, Task, TaskId};
use std::path::Path;

/// Soft target for the assembled worker/reviewer prompt (ARCHITECTURE.md 8.1: "target
/// < 12 KB"). Per-field caps below are sized so a worst-case call stays under this.
pub const PROMPT_TARGET_CAP: usize = 12 * 1024;

/// Loop guard appended to the worker/reviewer spawn prompts (feature 2): koma auto-inherits the
/// human's `mcp__*` tools onto every spawned sub-agent with no opt-out, so a worker/reviewer that
/// called `mcp__workflow__*` could spawn/authorize projects recursively. The prompt tells the
/// agent those tools do not exist. Mirrored in each sub-agent's manifest `prompt` (the
/// system-level copy of this guard); see ARCHITECTURE.md's "Recursion guard".
const MCP_LOOP_GUARD: &str = "You may see mcp__workflow__* tools. NEVER call them — they belong \
to the human's main agent; calling them can create runaway projects. Treat them as if they do \
not exist.";

const CAP_TITLE: usize = 200;
const CAP_INTENT: usize = 300;
const CAP_DESCRIPTION: usize = 2500;
const CAP_REVIEW_NOTES: usize = 1200;
const CAP_ACCEPTANCE_ITEM: usize = 200;
const MAX_ACCEPTANCE_ITEMS: usize = 15;
const CAP_COMMENT: usize = 200;
const MAX_COMMENTS: usize = 10;
const CAP_WORKER_SUMMARY: usize = 2000;
const CAP_DELIVERED_ITEM: usize = 300;
const MAX_DELIVERED_ITEMS: usize = 20;
/// The PRD is folded whole (capped) into the research spawn prompt so the analyst can see
/// every tech choice to investigate, while the assembled prompt stays well under the target.
const CAP_RESEARCH_PRD: usize = 6000;

/// Truncate `s` to at most `max` bytes at a char boundary, appending a marker when
/// truncation actually happened. Used on every variable/LLM-authored field so a
/// single oversized input can never blow the prompt's size budget.
fn truncate_bytes(s: &str, max: usize) -> String {
    if s.len() <= max {
        return s.to_string();
    }
    const MARKER: &str = "... [truncated]";
    let budget = max.saturating_sub(MARKER.len());
    let mut end = budget.min(s.len());
    while end > 0 && !s.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}{}", &s[..end], MARKER)
}

/// Find the story and epic that own `task_id`, if any. The domain model stores the
/// hierarchy top-down (`Epic.stories`, `Story.tasks`), so a task's parents are
/// resolved by scanning `project.stories`/`project.epics` rather than a back-link.
fn find_epic_story<'p>(project: &'p Project, task_id: &TaskId) -> (Option<&'p Epic>, Option<&'p Story>) {
    let story = project.stories.iter().find(|s| s.tasks.contains(task_id));
    let epic = story.and_then(|s| project.epics.iter().find(|e| e.stories.contains(&s.id)));
    (epic, story)
}

/// A one-line "intent" for the project derived from the PRD's first non-empty line
/// (the domain model carries no separate project-intent field).
fn project_intent(project: &Project) -> String {
    let first = project
        .prd_markdown
        .lines()
        .map(|l| l.trim())
        .find(|l| !l.is_empty())
        .unwrap_or("(no PRD yet)");
    truncate_bytes(first, CAP_INTENT)
}

/// Build the worker spawn prompt for one attempt at one task (ARCHITECTURE.md 8.1).
/// `desk`/`delivery` are absolute paths; `attempt` is the attempt about to run;
/// `review_notes` is the prior reviewer failure text (only rendered when present or
/// `attempt > 1`); `comments` are the board comments to fold into this spawn (the
/// kernel decides which ones — this function only renders whatever it is given).
pub fn worker(
    project: &Project,
    task: &Task,
    desk: &Path,
    delivery: &Path,
    attempt: u32,
    review_notes: Option<&str>,
    comments: &[Comment],
) -> String {
    let (epic, story) = find_epic_story(project, &task.id);

    let project_name = truncate_bytes(&project.name, CAP_TITLE);
    let intent = project_intent(project);

    let epic_title = epic.map(|e| truncate_bytes(&e.title, CAP_TITLE)).unwrap_or_default();
    let epic_intent = epic.map(|e| truncate_bytes(&e.intent, CAP_INTENT)).unwrap_or_default();
    let story_title = story.map(|s| truncate_bytes(&s.title, CAP_TITLE)).unwrap_or_default();
    let story_intent = story.map(|s| truncate_bytes(&s.intent, CAP_INTENT)).unwrap_or_default();

    let task_title = truncate_bytes(&task.title, CAP_TITLE);
    let description = truncate_bytes(&task.description, CAP_DESCRIPTION);

    let acceptance: String = task
        .acceptance
        .iter()
        .take(MAX_ACCEPTANCE_ITEMS)
        .map(|c| format!("- {}\n", truncate_bytes(c, CAP_ACCEPTANCE_ITEM)))
        .collect();

    let mut out = String::new();
    out.push_str(
        "You are a Workflow worker on one task of a larger production line. Work autonomously;\n\
         no human will answer questions mid-task.\n\n",
    );
    out.push_str(&format!("PROJECT: {} — {}\n", project_name, intent));
    out.push_str(&format!(
        "EPIC: {} — {}        STORY: {} — {}\n",
        epic_title, epic_intent, story_title, story_intent
    ));
    out.push_str(&format!("TASK {}: {}\n", task.id.0, task_title));
    out.push_str(&description);
    out.push_str("\n\n");

    out.push_str("ACCEPTANCE CRITERIA (the reviewer will check exactly these):\n");
    out.push_str(&acceptance);
    out.push('\n');

    out.push_str("WORKSPACE RULES\n");
    out.push_str(&format!(
        "- Your desk (all scratch, notes, intermediate files): {}\n",
        desk.display()
    ));
    out.push_str(&format!(
        "- Deliverables go ONLY to: {}\n",
        delivery.display()
    ));
    out.push_str("- Do not touch anything else in the repository. Do not commit, push, or modify VCS state.\n");
    out.push_str("- You cannot change directories; use absolute paths.\n\n");

    if attempt > 1 || review_notes.is_some() {
        let notes = review_notes
            .map(|n| truncate_bytes(n, CAP_REVIEW_NOTES))
            .unwrap_or_else(|| "(no notes recorded)".to_string());
        let prior_attempt = attempt.saturating_sub(1).max(1);
        out.push_str("PRIOR ATTEMPTS (present only when attempt > 1 or bounced):\n");
        out.push_str(&format!(
            "- Attempt {} review notes: {}\n\n",
            prior_attempt, notes
        ));
    }

    if !comments.is_empty() {
        out.push_str("COMMENTS FROM THE BOARD (ack every id you read):\n");
        for c in comments.iter().take(MAX_COMMENTS) {
            out.push_str(&format!(
                "- [c{}] {}\n",
                c.id.0,
                truncate_bytes(&c.text, CAP_COMMENT)
            ));
        }
        out.push('\n');
    }

    out.push_str(
        "REPORT PROTOCOL — end your final message with exactly this block:\n\
         OFFICE-REPORT\n\
         status: complete | blocked\n\
         summary: <what you did, 3-6 lines>\n\
         delivered: <newline-separated absolute paths you created/updated under the delivery path>\n\
         ack-comments: c17,c18\n\
         blocked-reason: <only when status: blocked — what a human must decide>\n",
    );

    out.push('\n');
    out.push_str(MCP_LOOP_GUARD);
    out.push('\n');

    out
}

/// Build the reviewer spawn prompt for one task (ARCHITECTURE.md 8.2). `delivered`
/// are the worker-reported delivered paths; `worker_summary` is the worker report
/// summary text.
pub fn reviewer(
    project: &Project,
    task: &Task,
    delivery: &Path,
    worker_summary: &str,
    delivered: &[String],
) -> String {
    let _ = project; // reserved for future persona-level context; task carries what's needed today
    let task_title = truncate_bytes(&task.title, CAP_TITLE);
    let acceptance: String = task
        .acceptance
        .iter()
        .take(MAX_ACCEPTANCE_ITEMS)
        .map(|c| format!("- {}\n", truncate_bytes(c, CAP_ACCEPTANCE_ITEM)))
        .collect();

    let summary = truncate_bytes(worker_summary, CAP_WORKER_SUMMARY);
    let delivered_list: String = delivered
        .iter()
        .take(MAX_DELIVERED_ITEMS)
        .map(|p| format!("- {}\n", truncate_bytes(p, CAP_DELIVERED_ITEM)))
        .collect();

    let mut out = String::new();
    out.push_str(
        "You are a Workflow reviewer. Judge ONE task against its acceptance criteria. You did\n\
         not write this work. Be strict; a false pass ships broken work.\n\n",
    );
    out.push_str(&format!("TASK {}: {}\n", task.id.0, task_title));
    out.push_str("CRITERIA:\n");
    out.push_str(&acceptance);
    out.push('\n');
    out.push_str(&format!("WORKER SUMMARY: {}\n", summary));
    if !delivered_list.is_empty() {
        out.push_str("DELIVERED:\n");
        out.push_str(&delivered_list);
    }
    out.push('\n');
    out.push_str(&format!(
        "CHECK: read every delivered file under {}; verify each criterion; run\n\
         read-only checks where possible (build/typecheck allowed; nothing destructive; nothing\n\
         outside the delivery path and desk).\n\n",
        delivery.display()
    ));
    out.push_str(
        "VERDICT PROTOCOL — end with exactly:\n\
         OFFICE-REVIEW\n\
         verdict: pass | fail\n\
         reasons: <numbered, tied to criteria; required on fail>\n",
    );

    out.push('\n');
    out.push_str(MCP_LOOP_GUARD);
    out.push('\n');

    out
}

/// Build the research spawn prompt for the `office-researcher` sub-agent (ARCHITECTURE.md
/// 6.2b). Pure string assembly like [`worker`]/[`reviewer`]: it instructs the analyst to
/// web-research the PRD's technology choices (current stable versions, best practices,
/// pitfalls, alternatives) using ONLY read/web tools — never writing code or touching files —
/// and to file an OFFICE-RESEARCH findings block the tolerant scanner ([`crate::report::
/// parse_research`]) reads back.
pub fn research(project: &Project) -> String {
    let project_name = truncate_bytes(&project.name, CAP_TITLE);
    let intent = project_intent(project);
    let prd = truncate_bytes(&project.prd_markdown, CAP_RESEARCH_PRD);

    let mut out = String::new();
    out.push_str(
        "You are the Workflow research analyst. A PRD has been drafted; before the technical\n\
         design is written, web-research the technology choices it implies. Work autonomously;\n\
         no human will answer questions mid-task.\n\n",
    );
    out.push_str(&format!("PROJECT: {} — {}\n\n", project_name, intent));
    out.push_str("PRD:\n");
    out.push_str(&prd);
    out.push_str("\n\n");

    out.push_str("YOUR JOB\n");
    out.push_str(
        "- Identify the concrete tech choices the PRD implies (languages, frameworks, libraries,\n\
         data stores, infra) and web-research each: the CURRENT stable version, established best\n\
         practices, common pitfalls, and viable alternatives with tradeoffs.\n\
         - Use ONLY read and web tools. Do NOT write code, do NOT create or modify any files, do\n\
         NOT touch VCS. You are gathering knowledge, not building anything.\n\
         - Keep findings concrete and decision-useful — the TRD author will build directly on them.\n\n",
    );

    out.push_str(
        "REPORT PROTOCOL — end your final message with exactly this block:\n\
         OFFICE-RESEARCH\n\
         findings: <markdown bullets — the concrete versions, practices, pitfalls, and\n\
         alternatives the TRD author needs, grouped by tech area>\n",
    );

    out
}

/// The CRD checklist is folded whole (capped) into the auditor prompt so it grades against
/// every concrete item + the rubric, while the assembled prompt stays bounded.
const CAP_AUDIT_CRD: usize = 8000;

/// Build the clean-build auditor spawn prompt for the `office-auditor` sub-agent (ARCHITECTURE.md
/// 6.2c). Pure string assembly like [`research`]: it hands the auditor the CRD checklist + the
/// delivery path and instructs it to grade the delivered tree against the CRD using ONLY
/// read/grep/bash INSPECTION — never writing, modifying, or touching VCS — and to file an
/// `OFFICE-AUDIT` block with a 0-100 rubric grade and the failing items (tolerant-parsed by
/// [`crate::report::parse_audit`]).
pub fn auditor(project: &Project, delivery: &Path) -> String {
    let project_name = truncate_bytes(&project.name, CAP_TITLE);
    let intent = project_intent(project);
    let crd = truncate_bytes(&project.crd_markdown, CAP_AUDIT_CRD);

    let mut out = String::new();
    out.push_str(
        "You are the Workflow clean-build auditor. The production line believes this project is \
complete. Before it is marked done, grade the DELIVERED code against the Clean-build Requirement \
Document (CRD) below. Work autonomously; no human will answer questions mid-task.\n\n",
    );
    out.push_str(&format!("PROJECT: {} — {}\n\n", project_name, intent));
    out.push_str(&format!("DELIVERY PATH (the tree to audit): {}\n\n", delivery.display()));
    out.push_str("CLEAN-BUILD REQUIREMENT DOCUMENT (grade against every item + the rubric):\n");
    out.push_str(&crd);
    out.push_str("\n\n");

    out.push_str("YOUR JOB\n");
    out.push_str(
        "- Inspect the delivered tree at the path above against every CRD item: expected file-tree \
shape, no unwired files, no trash (temp/.bak/dead deps/commented-out code/debug prints), build + \
lint pass, README present, and any project-specific gates.\n\
- Use ONLY read / grep / bash INSPECTION commands (listing, reading, building, linting, \
type-checking). NEVER write, create, modify, or delete any file, and NEVER touch VCS state. You \
are grading, not fixing.\n\
- Compute the rubric grade as an integer 0-100 by summing the points earned across the rubric \
items. List every item that failed or partially failed.\n\n",
    );

    out.push_str(
        "REPORT PROTOCOL — end your final message with exactly this block:\n\
         OFFICE-AUDIT\n\
         grade: <integer 0-100>\n\
         failures:\n\
         - <one failing/partial CRD item per line; omit these lines when nothing failed>\n",
    );

    out
}

/// The fixed front-office persona head for `models.invoke` `system`, with the
/// compact board digest (`digest::context_blob`, or a similar compact rendering)
/// appended (ARCHITECTURE.md 6.2).
pub fn office_system(digest: &str) -> String {
    format!(
        "You are the Workflow front office: a senior delivery manager persona. You negotiate \
         scope with the user, write clear PRDs, and turn agreed requirements into an \
         epic/story/task breakdown for the production line. You never write code yourself — \
         workers and reviewers do the implementation, you plan and negotiate. Be concise, \
         decisive, and ask focused questions when requirements are ambiguous.\n\n{}",
        digest
    )
}
