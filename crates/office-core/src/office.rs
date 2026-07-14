//! Front-office persona logic (ARCHITECTURE.md 6.2 / 6.3): pure prompt assembly for
//! the `models.invoke` persona, the transcript folding policy, the JSON breakdown
//! parser+validator, and the delivery-path authorization gate.
//!
//! Everything here is a pure function over a `Project` (or plain values). The kernel
//! (`kernel.rs`) drives these: it appends user turns, emits `InvokeModel` effects the
//! driver runs OFF the tick loop, and consumes the results as ordinary commands. No IO,
//! no host, no threads live in this module — "NO LLM in the control loop" means the
//! persona is reconstructed deterministically here and the model is only ever a
//! stateless text oracle the driver calls.
//!
//! ## Multi-turn on a single-shot API (6.2)
//! `models.invoke` is `system`+`prompt` only (no messages array), 32KB prompt cap, 25s
//! budget. We rebuild multi-turn ourselves: `office_summary` (rolling) + the newest
//! transcript turns + the output contract. When the assembled prompt would cross
//! [`FOLD_THRESHOLD`], the kernel first issues a summarize invoke ([`build_fold`]) that
//! compresses the oldest half into `office_summary` ([`apply_fold`]), then re-issues the
//! persona invoke. [`build_invoke`] additionally HARD-truncates at [`HARD_PROMPT_CAP`]
//! (dropping oldest turns first, then the summary) so a pathological transcript can
//! never produce a `prompt exceeds 32KB` host error.

use serde::Deserialize;
use std::path::{Path, PathBuf};

use crate::domain::{ChatAuthor, ChatMsg, Epic, EpicId, Project, ProjectPhase, Story, StoryId, Task, TaskId, TaskState};
use crate::graph;
use crate::machine::{step_project, ProjectTransition};
use crate::prompts;

/// Fold the transcript when the assembled persona prompt would exceed this (6.2). Kept
/// well under the hard cap so folding is proactive, not a last resort.
pub const FOLD_THRESHOLD: usize = 24 * 1024;

/// Hard guard: `models.invoke` rejects a prompt over 32KB (`prompt exceeds 32KB`,
/// EXTENSIONS.md:444). [`build_invoke`] never returns a prompt longer than this.
pub const HARD_PROMPT_CAP: usize = 32 * 1024;

/// Why an off-loop invoke was issued. Carried on the `InvokeModel` effect and echoed
/// back on `Command::InvokeResult` so the kernel applies each result deterministically
/// (6.2 / 5.1) without any persistent per-request bookkeeping.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum InvokePurpose {
    /// A front-office conversational reply. Result appended to the transcript.
    Persona,
    /// The JSON epic/story/task breakdown. Result parsed+validated; valid -> board.
    Breakdown,
    /// The single re-ask after a breakdown parse failure (6.3.2). A second failure
    /// surfaces to the user instead of looping.
    BreakdownReask,
    /// The summarize-and-fold call. Result replaces `office_summary`; then the pending
    /// persona invoke is re-issued.
    Fold,
}

// ---------------------------------------------------------------------------
// Persona prompt assembly + folding
// ---------------------------------------------------------------------------

/// A compact one-project board digest appended to the persona `system` head so the
/// office has ambient awareness of the project it is speaking for.
fn board_digest(p: &Project) -> String {
    let total = p.tasks.len();
    let done = p
        .tasks
        .iter()
        .filter(|t| matches!(t.state, TaskState::Done { .. }))
        .count();
    format!(
        "PROJECT CONTEXT\nname: {}\nphase: {}\ntasks: {} total, {} done\nprd: {}\n",
        p.name,
        phase_word(&p.phase),
        total,
        done,
        if p.prd_markdown.trim().is_empty() { "not written yet" } else { "drafted" }
    )
}

fn phase_word(phase: &ProjectPhase) -> &'static str {
    match phase {
        ProjectPhase::Drafting => "drafting",
        ProjectPhase::Ready => "ready",
        ProjectPhase::Running => "running",
        ProjectPhase::Interrupted => "interrupted",
        ProjectPhase::Halted { .. } => "halted",
        ProjectPhase::Done { .. } => "done",
    }
}

fn who_word(who: &ChatAuthor) -> &'static str {
    match who {
        ChatAuthor::User => "User",
        ChatAuthor::Office => "Office",
    }
}

fn render_turns(turns: &[&ChatMsg]) -> String {
    let mut s = String::new();
    for m in turns {
        s.push_str(who_word(&m.who));
        s.push_str(": ");
        s.push_str(&m.text);
        s.push('\n');
    }
    s
}

const PERSONA_CONTRACT: &str = "\nRespond as the Workflow front office: negotiate scope, \
answer clearly, and drive toward a PRD. Be concise and decisive.\n";

fn assemble(summary: &str, turns: &[&ChatMsg], new_user_msg: &str) -> String {
    let mut prompt = String::new();
    if !summary.trim().is_empty() {
        prompt.push_str("SUMMARY OF EARLIER CONVERSATION:\n");
        prompt.push_str(summary);
        prompt.push_str("\n\n");
    }
    prompt.push_str("CONVERSATION:\n");
    prompt.push_str(&render_turns(turns));
    if !new_user_msg.is_empty() {
        prompt.push_str("User: ");
        prompt.push_str(new_user_msg);
        prompt.push('\n');
    }
    prompt.push_str(PERSONA_CONTRACT);
    prompt
}

/// Truncate `s` to at most `max` bytes at a char boundary, appending a marker when it
/// actually truncated.
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

/// Build the persona `(system, prompt)` for `models.invoke` (6.2). `new_user_msg` is an
/// optional trailing user line (empty when the caller already appended it to the
/// transcript). The returned prompt is guaranteed `<= HARD_PROMPT_CAP`: oldest turns are
/// dropped first, and if the summary alone still overflows it is byte-truncated.
pub fn build_invoke(p: &Project, new_user_msg: &str) -> (String, String) {
    let system = prompts::office_system(&board_digest(p));
    let mut turns: Vec<&ChatMsg> = p.office_transcript.iter().collect();

    loop {
        let prompt = assemble(&p.office_summary, &turns, new_user_msg);
        if prompt.len() <= HARD_PROMPT_CAP {
            return (system, prompt);
        }
        if turns.is_empty() {
            // Even with no turns the prompt overflows: the summary is pathological.
            // Truncate it hard and re-assemble; guard the final size unconditionally.
            let summary = truncate_bytes(&p.office_summary, HARD_PROMPT_CAP / 2);
            let prompt = assemble(&summary, &[], new_user_msg);
            return (system, truncate_bytes(&prompt, HARD_PROMPT_CAP));
        }
        turns.remove(0); // drop the oldest turn and retry
    }
}

/// Whether the persona prompt for `new_user_msg` would cross [`FOLD_THRESHOLD`] and the
/// oldest half should be folded into the summary first (6.2).
pub fn should_fold(p: &Project, new_user_msg: &str) -> bool {
    if p.office_transcript.len() < 2 {
        return false; // nothing meaningful to fold
    }
    let turns: Vec<&ChatMsg> = p.office_transcript.iter().collect();
    assemble(&p.office_summary, &turns, new_user_msg).len() > FOLD_THRESHOLD
}

/// Build the summarize `(system, prompt)` that folds the oldest half of the transcript
/// (plus the existing summary) into a single terse summary (6.2).
pub fn build_fold(p: &Project) -> (String, String) {
    let half = p.office_transcript.len() / 2;
    let old: Vec<&ChatMsg> = p.office_transcript[..half].iter().collect();
    let system = "You compress a software requirements conversation. Keep every decision, \
agreed scope item, constraint, and open question. Output a terse summary only, no preamble."
        .to_string();
    let mut prompt = String::new();
    if !p.office_summary.trim().is_empty() {
        prompt.push_str("EXISTING SUMMARY:\n");
        prompt.push_str(&p.office_summary);
        prompt.push_str("\n\n");
    }
    prompt.push_str("CONVERSATION TO FOLD IN:\n");
    prompt.push_str(&render_turns(&old));
    prompt.push_str("\nProduce the merged summary.\n");
    (system, truncate_bytes(&prompt, HARD_PROMPT_CAP))
}

/// Apply a fold result: replace `office_summary` with the model's merged summary and
/// drop the oldest half of the transcript, keeping the newest turns verbatim (6.2).
pub fn apply_fold(p: &mut Project, summary: String) {
    let half = p.office_transcript.len() / 2;
    p.office_transcript.drain(0..half);
    p.office_summary = summary;
}

// ---------------------------------------------------------------------------
// Breakdown JSON parse + validate (6.3.2)
// ---------------------------------------------------------------------------

/// The output-contract instruction for the breakdown invoke: a strict JSON shape the
/// deterministic kernel can validate. `error` is appended verbatim on the single re-ask.
pub fn build_breakdown_prompt(p: &Project, reask_error: Option<&str>) -> (String, String) {
    let system = prompts::office_system(&board_digest(p));
    let mut prompt = String::new();
    prompt.push_str("Break the PRD below into an epic/story/task plan for the production line.\n\n");
    prompt.push_str("PRD:\n");
    prompt.push_str(&truncate_bytes(&p.prd_markdown, HARD_PROMPT_CAP / 2));
    prompt.push_str(
        "\n\nOutput ONLY JSON (no prose, no code fence) with this exact shape:\n\
{\"epics\":[{\"slug\":\"kebab\",\"title\":\"..\",\"intent\":\"..\",\"stories\":[\
{\"slug\":\"kebab\",\"title\":\"..\",\"intent\":\"..\",\"tasks\":[\
{\"slug\":\"kebab\",\"title\":\"..\",\"description\":\"..\",\"acceptance\":[\"..\"],\
\"priority\":0,\"blocked_by\":[\"other-task-slug\"]}]}]}]}\n\
Rules: every slug is unique and [a-z0-9-]; acceptance is non-empty; blocked_by lists task \
slugs only; the blocked_by graph is acyclic; add a blocked_by edge between tasks that write \
the same file.\n",
    );
    if let Some(err) = reask_error {
        prompt.push_str("\nYour previous answer was rejected: ");
        prompt.push_str(&truncate_bytes(err, 1000));
        prompt.push_str("\nReturn corrected JSON.\n");
    }
    (system, truncate_bytes(&prompt, HARD_PROMPT_CAP))
}

/// Why a breakdown JSON was rejected. Surfaced (quoted) on the single re-ask (6.3.2).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum BreakdownError {
    /// Not valid JSON in the expected shape.
    Json(String),
    /// No tasks at all — an empty plan is never accepted.
    Empty,
    /// A slug is `[a-z0-9-]`-invalid or empty.
    BadSlug(String),
    /// A slug (epic/story/task) appears more than once.
    DuplicateSlug(String),
    /// A task has an empty acceptance-criteria list.
    EmptyAcceptance(String),
    /// A `blocked_by` entry references a task slug that does not exist.
    UnknownRef(String),
    /// The `blocked_by` graph has a cycle. Carries the participating task slugs.
    Cycle(Vec<String>),
}

#[derive(Debug, Deserialize)]
struct RawBreakdown {
    #[serde(default)]
    epics: Vec<RawEpic>,
}

#[derive(Debug, Deserialize)]
struct RawEpic {
    slug: String,
    #[serde(default)]
    title: String,
    #[serde(default)]
    intent: String,
    #[serde(default)]
    stories: Vec<RawStory>,
}

#[derive(Debug, Deserialize)]
struct RawStory {
    slug: String,
    #[serde(default)]
    title: String,
    #[serde(default)]
    intent: String,
    #[serde(default)]
    tasks: Vec<RawTask>,
}

#[derive(Debug, Deserialize)]
struct RawTask {
    slug: String,
    #[serde(default)]
    title: String,
    #[serde(default)]
    description: String,
    #[serde(default)]
    acceptance: Vec<String>,
    #[serde(default)]
    priority: i32,
    #[serde(default)]
    blocked_by: Vec<String>,
}

/// A validated breakdown ready to land on the board. `slug`s are still the raw
/// LLM-authored slugs; [`apply_breakdown`] rebuilds the hierarchical domain ids with the
/// project prefix at apply time.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Breakdown {
    epics: Vec<VEpic>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct VEpic {
    slug: String,
    title: String,
    intent: String,
    stories: Vec<VStory>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct VStory {
    slug: String,
    title: String,
    intent: String,
    tasks: Vec<VTask>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct VTask {
    slug: String,
    title: String,
    description: String,
    acceptance: Vec<String>,
    priority: i32,
    blocked_by: Vec<String>,
}

fn valid_slug(s: &str) -> bool {
    !s.is_empty() && s.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
}

/// Strip surrounding prose / markdown code fences and isolate the outermost JSON object.
fn isolate_json(raw: &str) -> &str {
    let start = raw.find('{');
    let end = raw.rfind('}');
    match (start, end) {
        (Some(s), Some(e)) if e >= s => &raw[s..=e],
        _ => raw,
    }
}

/// Parse and validate a breakdown JSON blob (6.3.2): shape, slug validity + global
/// uniqueness (epics, stories, tasks share one namespace), non-empty acceptance, that
/// every `blocked_by` resolves, and that the task graph is acyclic (`graph.rs`).
pub fn parse_breakdown(raw: &str) -> Result<Breakdown, BreakdownError> {
    let json = isolate_json(raw);
    let parsed: RawBreakdown =
        serde_json::from_str(json).map_err(|e| BreakdownError::Json(e.to_string()))?;

    let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
    let mut task_slugs: std::collections::HashSet<String> = std::collections::HashSet::new();
    let mut epics: Vec<VEpic> = Vec::new();

    // First pass: shape/slug/acceptance validation + collect task slugs.
    for e in &parsed.epics {
        check_slug(&e.slug, &mut seen)?;
        let mut vstories = Vec::new();
        for s in &e.stories {
            check_slug(&s.slug, &mut seen)?;
            let mut vtasks = Vec::new();
            for t in &s.tasks {
                check_slug(&t.slug, &mut seen)?;
                task_slugs.insert(t.slug.clone());
                if t.acceptance.iter().all(|a| a.trim().is_empty()) {
                    return Err(BreakdownError::EmptyAcceptance(t.slug.clone()));
                }
                vtasks.push(VTask {
                    slug: t.slug.clone(),
                    title: t.title.clone(),
                    description: t.description.clone(),
                    acceptance: t.acceptance.iter().filter(|a| !a.trim().is_empty()).cloned().collect(),
                    priority: t.priority,
                    blocked_by: t.blocked_by.clone(),
                });
            }
            vstories.push(VStory {
                slug: s.slug.clone(),
                title: s.title.clone(),
                intent: s.intent.clone(),
                tasks: vtasks,
            });
        }
        epics.push(VEpic {
            slug: e.slug.clone(),
            title: e.title.clone(),
            intent: e.intent.clone(),
            stories: vstories,
        });
    }

    if task_slugs.is_empty() {
        return Err(BreakdownError::Empty);
    }

    // Second pass: every blocked_by must resolve to a known task slug.
    for e in &epics {
        for s in &e.stories {
            for t in &s.tasks {
                for dep in &t.blocked_by {
                    if !task_slugs.contains(dep) {
                        return Err(BreakdownError::UnknownRef(dep.clone()));
                    }
                }
            }
        }
    }

    // Acyclicity: build slug-level tasks and reuse the kernel's Kahn validator.
    let flat: Vec<Task> = epics
        .iter()
        .flat_map(|e| e.stories.iter())
        .flat_map(|s| s.tasks.iter())
        .map(|t| slug_task(t))
        .collect();
    if let Err(cycle) = graph::validate_acyclic(&flat) {
        return Err(BreakdownError::Cycle(cycle.nodes.into_iter().map(|id| id.0).collect()));
    }

    Ok(Breakdown { epics })
}

fn check_slug(slug: &str, seen: &mut std::collections::HashSet<String>) -> Result<(), BreakdownError> {
    if !valid_slug(slug) {
        return Err(BreakdownError::BadSlug(slug.to_string()));
    }
    if !seen.insert(slug.to_string()) {
        return Err(BreakdownError::DuplicateSlug(slug.to_string()));
    }
    Ok(())
}

/// A throwaway `Task` keyed by bare slug, used only to run the acyclicity check.
fn slug_task(t: &VTask) -> Task {
    Task {
        id: TaskId(t.slug.clone()),
        title: t.title.clone(),
        description: String::new(),
        acceptance: Vec::new(),
        blocked_by: t.blocked_by.iter().map(|d| TaskId(d.clone())).collect(),
        priority: t.priority,
        state: TaskState::Todo,
        bounces: 0,
        comments: Vec::new(),
        desk: None,
        last_report: None,
        last_review: None,
        history: Vec::new(),
    }
}

/// Land a validated breakdown on the project's board and move `Drafting -> Ready`
/// (6.3.2). Domain ids are rebuilt hierarchically with the project prefix
/// (`<project>/<epic>/<story>/<task>`); `blocked_by` slugs are resolved to full ids.
/// Every task lands `Todo` (groomed by the breakdown); the ready-set gates dispatch.
pub fn apply_breakdown(p: &mut Project, b: Breakdown) {
    let proj = p.id.0.clone();

    // slug -> full TaskId map for blocked_by resolution.
    let mut task_id: std::collections::HashMap<String, TaskId> = std::collections::HashMap::new();
    for e in &b.epics {
        for s in &e.stories {
            for t in &s.tasks {
                let full = format!("{}/{}/{}/{}", proj, e.slug, s.slug, t.slug);
                task_id.insert(t.slug.clone(), TaskId(full));
            }
        }
    }

    let mut epics = Vec::new();
    let mut stories = Vec::new();
    let mut tasks = Vec::new();

    for e in b.epics {
        let epic_id = EpicId(format!("{}/{}", proj, e.slug));
        let mut story_ids = Vec::new();
        for s in e.stories {
            let story_id = StoryId(format!("{}/{}/{}", proj, e.slug, s.slug));
            let mut task_ids = Vec::new();
            for t in s.tasks {
                let id = task_id.get(&t.slug).cloned().unwrap_or(TaskId(t.slug.clone()));
                let blocked_by = t
                    .blocked_by
                    .iter()
                    .filter_map(|d| task_id.get(d).cloned())
                    .collect();
                task_ids.push(id.clone());
                tasks.push(Task {
                    id,
                    title: t.title,
                    description: t.description,
                    acceptance: t.acceptance,
                    blocked_by,
                    priority: t.priority,
                    state: TaskState::Todo,
                    bounces: 0,
                    comments: Vec::new(),
                    desk: None,
                    last_report: None,
                    last_review: None,
                    history: Vec::new(),
                });
            }
            story_ids.push(story_id.clone());
            stories.push(Story {
                id: story_id,
                title: s.title,
                intent: s.intent,
                tasks: task_ids,
            });
        }
        epics.push(Epic {
            id: epic_id,
            title: e.title,
            intent: e.intent,
            stories: story_ids,
        });
    }

    p.epics = epics;
    p.stories = stories;
    p.tasks = tasks;

    if let Ok(phase) = step_project(&p.phase, ProjectTransition::AcceptBreakdown) {
        p.phase = phase;
    }
}

// ---------------------------------------------------------------------------
// Authorization (6.3.3)
// ---------------------------------------------------------------------------

/// Why authorization was refused. The delivery-path gate is absolute (6.3.3): no valid
/// path means the project CANNOT leave `Ready`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum AuthError {
    /// The delivery path is not absolute.
    NotAbsolute,
    /// No bound-session workspace is known, so containment cannot be verified.
    NoWorkspace,
    /// The path is outside the bound-session workspace and the escape hatch is off.
    OutsideWorkspace,
    /// The project is not in `Ready` (nothing to authorize).
    WrongPhase,
}

/// The pure delivery-path check (6.3.3): absolute, and inside the bound-session
/// workspace unless `allow_outside` is set (the documented escape hatch). `mkdir -p` is
/// the driver's job (IO); this only validates the shape.
pub fn validate_delivery_path(
    path: &Path,
    workspace: Option<&Path>,
    allow_outside: bool,
) -> Result<(), AuthError> {
    if !path.is_absolute() {
        return Err(AuthError::NotAbsolute);
    }
    if allow_outside {
        return Ok(());
    }
    match workspace {
        None => Err(AuthError::NoWorkspace),
        Some(ws) if path.starts_with(ws) => Ok(()),
        Some(_) => Err(AuthError::OutsideWorkspace),
    }
}

/// Authorize a `Ready` project to start grinding (6.3.3): validate the delivery path,
/// record it, and transition `Ready -> Running`. Pure — the driver has already `mkdir
/// -p`'d the path before calling. Any validation failure leaves the project untouched.
pub fn authorize(p: &mut Project, delivery: PathBuf, allow_outside: bool) -> Result<(), AuthError> {
    validate_delivery_path(&delivery, p.workspace.as_deref(), allow_outside)?;
    let phase = step_project(&p.phase, ProjectTransition::Authorize { delivery_path_valid: true })
        .map_err(|_| AuthError::WrongPhase)?;
    p.delivery_path = Some(delivery);
    p.phase = phase;
    Ok(())
}
