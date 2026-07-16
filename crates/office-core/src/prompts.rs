//! Worker/reviewer spawn prompt assembly and the front-office persona head.
//!
//! Every builder here is a pure string function: same inputs -> same output. The
//! variable, LLM-authored sections (description, review notes, comment text,
//! acceptance criteria) are byte-capped BEFORE assembly so the whole prompt stays
//! comfortably bounded even when fed pathological (oversized) input — see
//! ARCHITECTURE.md 8.

use crate::domain::{AgentBinding, Comment, Epic, Project, Story, Task, TaskId, TaskState};
use std::path::Path;

/// Soft target for the assembled worker/reviewer prompt (ARCHITECTURE.md 8.1: "target
/// < 12 KB"). Per-field caps below are sized so a worst-case call stays under this.
pub const PROMPT_TARGET_CAP: usize = 12 * 1024;

/// The clean-build hygiene contract folded into BOTH the worker and reviewer prompts (item 2):
/// the tree must never accumulate vendored deps, build artifacts, temp files, or editor droppings,
/// and a README is required. Per-task reviewers enforce it (nobody owned tree hygiene until the
/// final audit before this — a project passed every per-task review then failed the audit at 30/100
/// on `node_modules/`, `.vite/`, SQLite WAL files, and a missing README).
const HYGIENE_RULES: &str = "CLEAN-BUILD HYGIENE — the delivered tree must stay clean:\n\
- NO dependency or build artifacts committed: node_modules/, vendor/, target/, dist/, build/, .vite/, \
.next/, __pycache__/, *.pyc, coverage/. Install/build them from a manifest (package.json / Cargo.toml / \
requirements.txt); never vendor them into the tree.\n\
- NO temp / editor / runtime droppings: *.tmp, *.bak, *.orig, *.swp, *.log, .DS_Store, and SQLite \
WAL/journal sidecars (*.db-wal, *.db-shm, *.sqlite-journal).\n\
- NO commented-out dead code, no stray debug prints, no unwired/orphan files.\n\
- A README at the delivery root is REQUIRED.\n";

/// Byte cap for the `git diff --stat` block folded into the reviewer prompt (item 2).
const CAP_DIFF_STAT: usize = 2000;
/// How many sibling tasks the reviewer board digest lists per bucket (item 2).
const MAX_DIGEST_ITEMS: usize = 12;

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
/// Byte cap for the office-notes block folded into the worker prompt (sprints item 4): stack
/// research + prior-sprint learnings. Bounded so a long-running multi-sprint project's accreted
/// notes never blow the worker prompt budget.
const CAP_WORKER_NOTES: usize = 3000;
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
    if project.worktree_desks {
        // Worktree desks (item 1/2): the desk IS a full git worktree of the delivery repo and THE
        // working root. The worker edits the tree in place; the office branches / commits / merges it.
        // The hard truth the worker must know (koma can't scope a sub-agent's cwd to the desk): the
        // shell cwd is workspace [0] = the SHARED delivery checkout, NOT this desk, and it can't be
        // changed — so a relative path lands in the WRONG tree. Absolute desk paths only.
        out.push_str(&format!(
            "- Your desk is a full git worktree of the delivery repo, and it IS your workspace: {}\n",
            desk.display()
        ));
        out.push_str(
            "- Work DIRECTLY in that tree. The files you create and edit there ARE the deliverables — \
there is NO separate copy step and NO separate delivery path.\n",
        );
        out.push_str(&format!(
            "- CRITICAL — where files land: your shell's working directory is NOT this desk and you \
CANNOT change it. A relative path (or workspace root [0]) resolves to a DIFFERENT checkout — the \
shared delivery working tree — not to your desk. Writing there is WRONG: it pollutes a tree you do \
not own, your desk stays empty, and the task is bounced. ALWAYS address files by their ABSOLUTE path \
under {}. NEVER write to any other workspace root, and NEVER write to the delivery path or workspace \
[0].\n",
            desk.display()
        ));
        out.push_str(
            "- NEVER run git — the office owns ALL VCS operations (branch, commit, merge). Do not init, \
add, commit, checkout, branch, stash, reset, or touch .git in any way.\n\n",
        );
    } else {
        out.push_str(&format!(
            "- Your desk (all scratch, notes, intermediate files): {}\n",
            desk.display()
        ));
        out.push_str(&format!("- Deliverables go ONLY to: {}\n", delivery.display()));
        out.push_str(
            "- Do not touch anything else in the repository. NEVER run git — the office owns all VCS \
operations (commits, branches, merges).\n",
        );
        out.push_str("- You cannot change directories; use absolute paths.\n\n");
    }

    // Tree hygiene (item 2): the worker must not seed the trash the reviewer would fail it for.
    out.push_str(HYGIENE_RULES);
    out.push('\n');

    // Researcher context feed (sprints item 4): the stack research + accreted prior-sprint learnings
    // live in `research_notes`; fold them in (bounded) so each sprint's workers build on what the
    // office learned. Non-empty guarded, so a project without notes is byte-identical to before.
    if !project.research_notes.trim().is_empty() {
        out.push_str("OFFICE NOTES (stack research + prior-sprint learnings — apply where relevant):\n");
        out.push_str(&truncate_bytes(&project.research_notes, CAP_WORKER_NOTES));
        out.push_str("\n\n");
    }

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

    // Board digest (item 2): the sibling tasks so the reviewer does NOT flag "missing X" that a
    // still-in-flight sibling owns, and DOES catch a collision with already-merged work.
    out.push_str(&review_board_digest(project, &task.id));
    out.push('\n');

    if project.worktree_desks {
        // Worktree desks (item 2): review the INTEGRATED tree in the task worktree + the branch
        // diff against main. The reviewer can build/typecheck the WHOLE product with this task's
        // changes present, which per-task scratch desks could not.
        let tree = task
            .desk
            .as_ref()
            .map(|d| d.display().to_string())
            .unwrap_or_else(|| delivery.display().to_string());
        out.push_str(&format!(
            "CHECK: this task's work lives in its git worktree at {tree} — the FULL product tree with \
this task's changes present (its branch is not yet merged into main). That worktree IS your \
workspace. Read the changed files, verify each criterion, and run read-only checks (build / \
typecheck / lint) against the whole tree. Nothing destructive; NEVER run git (the office owns VCS).\n"
        ));
        out.push_str(&format!(
            "- Your shell's working directory is NOT this worktree and cannot be changed: a relative \
path (or workspace root [0]) resolves to a DIFFERENT checkout (the shared delivery tree), not to \
{tree}. Address every file by its ABSOLUTE path under {tree}, run any build/typecheck there, and \
never write outside it — especially not to the delivery path or workspace [0].\n"
        ));
        if let Some(stat) = task.diff_stat.as_deref() {
            let stat = truncate_bytes(stat.trim(), CAP_DIFF_STAT);
            if !stat.is_empty() {
                out.push_str("\nTASK BRANCH DIFF vs main (git diff --stat, capped):\n");
                out.push_str(&stat);
                out.push('\n');
            }
        }
        out.push('\n');
    } else {
        out.push_str(&format!(
            "CHECK: read every delivered file under {}; verify each criterion; run\n\
             read-only checks where possible (build/typecheck allowed; nothing destructive; nothing\n\
             outside the delivery path and desk).\n\n",
            delivery.display()
        ));
    }

    // Tree-hygiene gate (item 2): the per-task reviewer OWNS tree cleanliness — the final audit
    // must never be the first to notice trash.
    out.push_str(HYGIENE_RULES);
    out.push_str(
        "FAIL the task if it introduces ANY of the trash above, leaves unwired/orphan files, or omits a \
required README.\n\n",
    );

    out.push_str(
        "VERDICT PROTOCOL — end with exactly:\n\
         OFFICE-REVIEW\n\
         verdict: pass | fail\n\
         reasons: <numbered, tied to criteria; required on fail>\n\
         hygiene: <0-100 clean-build score for THIS task's changes; 100 = spotless, deduct for any \
trash, dead files, or missing README>\n",
    );

    out.push('\n');
    out.push_str(MCP_LOOP_GUARD);
    out.push('\n');

    out
}

/// Render the BOARD DIGEST paragraph for the reviewer prompt (item 2): which sibling tasks are
/// already MERGED (Done) and which are IN FLIGHT (with the worker persona that owns them), so the
/// reviewer neither flags a gap a sibling owns nor waves through a collision. Data is read straight
/// off the board; both buckets are capped so the paragraph stays bounded.
fn review_board_digest(project: &Project, current: &TaskId) -> String {
    let mut merged: Vec<String> = Vec::new();
    let mut in_flight: Vec<String> = Vec::new();
    for t in &project.tasks {
        if &t.id == current {
            continue;
        }
        match &t.state {
            TaskState::Done { .. } => merged.push(digest_label(t)),
            TaskState::OnProgress { binding, .. } => {
                in_flight.push(format!("{} (worked by {})", digest_label(t), owner_name(binding, &t.id)))
            }
            TaskState::Review { .. } => in_flight.push(format!("{} (in review)", digest_label(t))),
            _ => {}
        }
    }

    let mut out = String::from(
        "BOARD DIGEST (siblings on the line — do NOT flag work another task owns, and DO catch a \
collision with already-merged work):\n",
    );
    out.push_str(&format!("- merged: {}\n", digest_join(&merged)));
    out.push_str(&format!("- in flight: {}\n", digest_join(&in_flight)));
    out
}

/// A short "`<task-slug>` — `<title>`" label for one sibling in the board digest.
fn digest_label(t: &Task) -> String {
    let slug = t.id.0.rsplit('/').next().unwrap_or(&t.id.0);
    let title = truncate_bytes(t.title.trim(), 60);
    if title.is_empty() {
        slug.to_string()
    } else {
        format!("{slug} — {title}")
    }
}

/// The worker persona that owns an in-flight sibling (item 2): the binding's stamped persona short
/// name, or the deterministic assignment from the task id when the binding predates personas.
fn owner_name(binding: &AgentBinding, task_id: &TaskId) -> String {
    crate::persona::short_worker_name(&binding.persona)
        .map(str::to_string)
        .unwrap_or_else(|| crate::persona::worker_persona(&task_id.0).to_string())
}

/// Join a capped digest bucket into a single line, appending "(+N more)" when truncated; "(none)"
/// when empty.
fn digest_join(items: &[String]) -> String {
    if items.is_empty() {
        return "(none)".to_string();
    }
    let shown: Vec<String> = items.iter().take(MAX_DIGEST_ITEMS).cloned().collect();
    let mut joined = shown.join("; ");
    if items.len() > MAX_DIGEST_ITEMS {
        joined.push_str(&format!("; (+{} more)", items.len() - MAX_DIGEST_ITEMS));
    }
    joined
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
        "- Per-task clean-build hygiene was ALREADY enforced at each merge — every reviewer graded the \
tree it merged for trash, dead files, and a README (item 3). Grade the INTEGRATED whole here: \
cross-task consistency, wiring between merged pieces, and anything a single-task review could not see.\n\
- Inspect the delivered tree at the path above against every CRD item: expected file-tree \
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
