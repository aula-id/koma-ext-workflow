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
    /// The JSON epic/story/task breakdown. Result parsed+validated; valid -> board. An Err
    /// whose text is a `models.invoke` timeout falls back to ONE [`BreakdownCompact`]
    /// attempt instead of surfacing immediately (6.3.2 timeout ladder); any other Err, or a
    /// parse failure, is unchanged (parse failure -> [`BreakdownReask`]).
    Breakdown,
    /// The single re-ask after a breakdown parse failure (6.3.2). A second failure
    /// surfaces to the user instead of looping.
    BreakdownReask,
    /// The compact fallback after a FIRST-ATTEMPT breakdown timeout (6.3.2 timeout ladder):
    /// a smaller, cheaper ask (`build_breakdown_prompt(.., compact: true)`) issued once the
    /// driver's own pool-level retry (`driver.rs on_invoke_done`) is already exhausted. Any
    /// failure here — timeout, other error, or a parse rejection — is terminal: it surfaces
    /// an actionable notice rather than looping further.
    BreakdownCompact,
    /// The summarize-and-fold call. Result replaces `office_summary`; then the pending
    /// persona invoke is re-issued.
    Fold,
    /// The Technical Requirements Document authoring call (ARCHITECTURE.md 6.2b), issued after
    /// web-research (or a research degrade) lands during Drafting. `Ok` with a ```trd fence ->
    /// store `trd_markdown` and run the safeguard gate then the CRD; a missing fence or any
    /// `Err` STILL proceeds to the CRD (from the PRD alone) — Drafting never wedges on a TRD
    /// failure.
    Trd,
    /// The Clean-build Requirement Document authoring call (ARCHITECTURE.md 6.2c), issued after
    /// the TRD (and its safeguard gate). `Ok` with a ```crd fence -> store `crd_markdown`, run
    /// the safeguard gate, then request the breakdown; a missing fence or any `Err` STILL
    /// requests the breakdown — the project just completes without a clean-build audit.
    Crd,
    /// The safeguard no-assume gate over the PRD (ARCHITECTURE.md 6.2c). Runs on the
    /// `safeguard_role`. `clean` -> proceed to research; `assumptions` -> stop with
    /// `pending_assumptions`; `Err` -> FAIL-OPEN (proceed). Copy/Eq is preserved by using a
    /// distinct variant per doc instead of a payload.
    AssumeCheckPrd,
    /// The safeguard no-assume gate over the TRD (6.2c). `clean` -> proceed to the CRD invoke.
    AssumeCheckTrd,
    /// The safeguard no-assume gate over the CRD (6.2c). `clean` -> proceed to the breakdown.
    AssumeCheckCrd,
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

/// The no-assume safeguard clause (ARCHITECTURE.md 6.2c) appended to every doc-authoring
/// contract — PRD ([`PERSONA_CONTRACT`]), TRD ([`build_trd_prompt`]), and CRD
/// ([`build_crd_prompt`]). Every choice the user did not state, research did not ground, and the
/// user did not delegate must live under "Open questions", never silently in the doc body — this
/// is what the safeguard gate ([`build_assume_check_prompt`]) then verifies.
pub const NO_ASSUME_CLAUSE: &str = "\nDo NOT assume anything the user did not state. Every \
choice that is not user-stated, research-grounded, or explicitly delegated ('you decide' / 'up \
to you') belongs under an 'Open questions' section, never in the doc body. Record delegated \
choices as 'Delegated decision: ...'.\n";

const PERSONA_CONTRACT: &str = "\nRespond as the Workflow front office: negotiate scope, \
answer clearly, and drive toward a PRD. Be concise and decisive.\n\
When (and only when) the scope is agreed, emit the COMPLETE PRD as markdown inside a \
fenced block that starts with ```prd and ends with ``` — that exact fence is how the \
system captures the PRD and starts the production line; a PRD outside that fence does \
not count and nothing will happen.\n\
Do NOT assume anything the user did not state. Every choice that is not user-stated, \
research-grounded, or explicitly delegated ('you decide' / 'up to you') belongs under an \
'Open questions' section, never in the PRD body. Record delegated choices as 'Delegated \
decision: ...'.\n";

/// Capture the LAST ` ```<tag> ` fenced block from a persona reply, if any — the generalized
/// engine behind both PRD (6.2) and TRD (6.2b) capture. The fence is the explicit capture
/// contract given to the persona (PRD: [`PERSONA_CONTRACT`]; TRD: [`build_trd_prompt`]);
/// free-text 'here is the doc' prose is deliberately ignored. `tag` is the language hint
/// after the opening backticks (`"prd"` / `"trd"`), matched exactly.
pub fn extract_fenced(reply: &str, tag: &str) -> Option<String> {
    let fence = format!("```{tag}");
    let flen = fence.len();
    let mut result = None;
    let mut rest = reply;
    while let Some(start) = rest.find(&fence) {
        let after = &rest[start + flen..];
        // fence marker must end its line
        let body_start = match after.find('\n') {
            Some(i) if after[..i].trim().is_empty() => i + 1,
            _ => {
                rest = &rest[start + flen..];
                continue;
            }
        };
        let body = &after[body_start..];
        match body.find("\n```") {
            Some(end) => {
                let doc = body[..end].trim();
                if !doc.is_empty() {
                    result = Some(doc.to_string());
                }
                rest = &body[end + 4..];
            }
            None => {
                // unterminated fence: tolerate, take the remainder
                let doc = body.trim();
                if !doc.is_empty() {
                    result = Some(doc.to_string());
                }
                break;
            }
        }
    }
    result
}

/// Capture a PRD from a persona reply: the LAST ```prd fenced block, if any (6.2). A thin
/// wrapper over [`extract_fenced`] so the fence-capture contract stays in one place.
pub fn extract_prd(reply: &str) -> Option<String> {
    extract_fenced(reply, "prd")
}

/// Byte cap for stored `research_notes` (ARCHITECTURE.md 6.2b): the findings are folded into
/// the TRD prompt (capped again there) and carried on the panel snapshot, so a runaway
/// researcher can never balloon the state file. Truncated with a marker on write.
pub const RESEARCH_NOTES_CAP: usize = 16 * 1024;

/// Extract the researcher's findings from a spawn result (6.2b): the tolerant OFFICE-RESEARCH
/// `findings:` block if present ([`crate::report::parse_research`]), else the whole reply text.
/// Capped to [`RESEARCH_NOTES_CAP`] with a truncation marker — this IS the "truncate on write"
/// enforcement for `Project.research_notes`.
pub fn extract_research(text: &str) -> String {
    let raw = crate::report::parse_research(text).unwrap_or_else(|| text.trim().to_string());
    truncate_bytes(&raw, RESEARCH_NOTES_CAP)
}

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
/// `compact` (the timeout retry ladder, 6.3.2) additionally caps the plan to at most 6
/// tasks total in one epic with short titles/descriptions and at most 2 acceptance bullets
/// per task — a deliberately smaller ask that is cheaper and faster for the model to answer
/// than the full contract, so a slow/timing-out model gets one narrower shot instead of the
/// same prompt again. The non-compact contract is otherwise byte-for-byte unchanged.
pub fn build_breakdown_prompt(p: &Project, reask_error: Option<&str>, compact: bool) -> (String, String) {
    let system = prompts::office_system(&board_digest(p));
    let mut prompt = String::new();
    prompt.push_str("Break the PRD below into an epic/story/task plan for the production line.\n\n");
    prompt.push_str("PRD:\n");
    prompt.push_str(&truncate_bytes(&p.prd_markdown, HARD_PROMPT_CAP / 3));
    // The TRD (when present, 6.2b) carries the concrete stack/versions/architecture the plan
    // must honor; fold it in alongside the PRD (compact mode gets it too).
    if !p.trd_markdown.trim().is_empty() {
        prompt.push_str("\n\nTRD (technical requirements — honor these choices):\n");
        prompt.push_str(&truncate_bytes(&p.trd_markdown, HARD_PROMPT_CAP / 3));
    }
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
    if compact {
        prompt.push_str(
            "\nCOMPACT MODE (the previous attempt timed out): keep this breakdown as small \
as possible — at most 6 tasks TOTAL, in exactly one epic. Titles must be short (a few words); \
descriptions must be one line each; acceptance is at most 2 bullets per task. Output JSON \
ONLY — no prose, no code fences, nothing outside the JSON object.\n",
        );
    }
    if let Some(err) = reask_error {
        prompt.push_str("\nYour previous answer was rejected: ");
        prompt.push_str(&truncate_bytes(err, 1000));
        prompt.push_str("\nReturn corrected JSON.\n");
    }
    (system, truncate_bytes(&prompt, HARD_PROMPT_CAP))
}

/// Build the TRD authoring `(system, prompt)` for `models.invoke` (ARCHITECTURE.md 6.2b). The
/// prompt is the PRD (capped) plus the web-research findings when present (capped), then the
/// output contract: a COMPLETE Technical Requirements Document inside a ```trd fenced block.
/// Pure and byte-bounded exactly like [`build_breakdown_prompt`].
pub fn build_trd_prompt(p: &Project) -> (String, String) {
    let system = prompts::office_system(&board_digest(p));
    let mut prompt = String::new();
    prompt.push_str("Draft the Technical Requirements Document (TRD) for the PRD below.\n\n");
    prompt.push_str("PRD:\n");
    prompt.push_str(&truncate_bytes(&p.prd_markdown, HARD_PROMPT_CAP / 3));
    if !p.research_notes.trim().is_empty() {
        prompt.push_str("\n\nRESEARCH FINDINGS (web-researched stack notes — weigh these):\n");
        prompt.push_str(&truncate_bytes(&p.research_notes, HARD_PROMPT_CAP / 4));
    }
    prompt.push_str(
        "\n\nEmit the COMPLETE Technical Requirements Document as markdown inside a fenced block \
that starts with ```trd and ends with ``` — that exact fence is how the system captures the \
TRD. Cover, as sections: technology stack with SPECIFIC current stable versions, architecture, \
data model, API surface, testing strategy, deployment, and constraints. Be concrete and \
decisive; this document drives the epic/story/task breakdown.\n",
    );
    prompt.push_str(NO_ASSUME_CLAUSE);
    (system, truncate_bytes(&prompt, HARD_PROMPT_CAP))
}

/// Build the CRD authoring `(system, prompt)` for `models.invoke` (ARCHITECTURE.md 6.2c). The
/// CRD is a Clean-build Requirement Document: a concrete, gradeable acceptance checklist for
/// THIS project's delivered tree — expected file-tree shape, no unwired files (modules nothing
/// imports), no trash (temp/`.bak`/dead deps/commented-out code/debug prints), build + lint
/// pass, a README present — PLUS a grading rubric whose point weights sum to 100. It drives the
/// completion-time auditor. Built from the PRD (+ TRD when present), byte-bounded like
/// [`build_trd_prompt`].
pub fn build_crd_prompt(p: &Project) -> (String, String) {
    let system = prompts::office_system(&board_digest(p));
    let mut prompt = String::new();
    prompt.push_str("Draft the Clean-build Requirement Document (CRD) for the project below.\n\n");
    prompt.push_str("PRD:\n");
    prompt.push_str(&truncate_bytes(&p.prd_markdown, HARD_PROMPT_CAP / 3));
    if !p.trd_markdown.trim().is_empty() {
        prompt.push_str("\n\nTRD (technical requirements — the CRD must match these choices):\n");
        prompt.push_str(&truncate_bytes(&p.trd_markdown, HARD_PROMPT_CAP / 3));
    }
    prompt.push_str(
        "\n\nEmit the COMPLETE Clean-build Requirement Document as markdown inside a fenced block \
that starts with ```crd and ends with ``` — that exact fence is how the system captures the CRD. \
It is the checklist a read-only auditor will grade the delivered code against, so make every \
item concrete and checkable by inspecting the delivered tree. Cover, as sections:\n\
- Expected file-tree shape: the directories/files a correct delivery must contain (and must NOT).\n\
- No unwired files: every module/file is imported/used by something; nothing dangling.\n\
- No trash: no temp/scratch/`.bak` files, no dead dependencies, no commented-out code, no debug \
prints/logging left in.\n\
- Build + lint: the project builds and lints/type-checks clean with the stack's standard tooling.\n\
- Docs: a README (or equivalent) is present and describes setup/run.\n\
- Any project-specific correctness gates implied by the PRD/TRD.\n\
Then a 'Grading rubric' section: a bulleted list of checks, each with an explicit point weight, \
whose weights SUM TO EXACTLY 100.\n",
    );
    prompt.push_str(NO_ASSUME_CLAUSE);
    (system, truncate_bytes(&prompt, HARD_PROMPT_CAP))
}

/// Build the safeguard no-assume gate `(system, prompt)` for one drafting doc (ARCHITECTURE.md
/// 6.2c). Given ONLY the user's own chat turns (transcript `User` entries) + the research notes
/// + the doc itself, the safeguard lists every choice in the doc that the user did NOT state,
/// research did NOT ground, and the user did NOT explicitly delegate. `doc_label` is the human
/// label ("PRD"/"TRD"/"CRD"); `doc_body` is that doc's markdown. Pure + byte-bounded; the caller
/// (kernel) emits it on the `safeguard_role`.
pub fn build_assume_check_prompt(p: &Project, doc_label: &str, doc_body: &str) -> (String, String) {
    let system = "You are a strict requirements safeguard. Your ONE job is to catch ungrounded \
assumptions: choices a document asserts that the user never stated, that were not established by \
research, and that the user did not explicitly delegate. You do not rewrite the document; you \
only audit it for unapproved assumptions. Be precise and terse."
        .to_string();

    // Only the user's OWN turns are ground truth — the office's prior replies are not (an
    // assumption the office already made is exactly what we are trying to catch).
    let user_turns: String = p
        .office_transcript
        .iter()
        .filter(|m| matches!(m.who, ChatAuthor::User))
        .map(|m| format!("- {}\n", m.text))
        .collect();

    let mut prompt = String::new();
    prompt.push_str("USER STATEMENTS (the ONLY things the user actually said — ground truth):\n");
    if user_turns.trim().is_empty() {
        prompt.push_str("(none recorded)\n");
    } else {
        prompt.push_str(&truncate_bytes(&user_turns, HARD_PROMPT_CAP / 4));
    }
    if !p.research_notes.trim().is_empty() {
        prompt.push_str("\nRESEARCH FINDINGS (also count as grounded):\n");
        prompt.push_str(&truncate_bytes(&p.research_notes, HARD_PROMPT_CAP / 4));
    }
    prompt.push_str(&format!("\n{} UNDER REVIEW:\n", doc_label));
    prompt.push_str(&truncate_bytes(doc_body, HARD_PROMPT_CAP / 3));
    prompt.push_str(
        "\n\nList every choice in the document that the user did NOT state, research did NOT \
ground, and the user did NOT explicitly delegate ('you decide' / 'up to you' / recorded as a \
'Delegated decision'). A delegated or research-grounded choice is NOT an assumption. Output \
ONLY this block, nothing else:\n\
ASSUME-CHECK\n\
verdict: clean | assumptions\n\
- <one ungrounded assumption per line; omit these lines entirely when verdict is clean>\n",
    );
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
