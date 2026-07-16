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
//! `models.invoke` is `system`+`prompt` only (no messages array), 32KB prompt cap, and a
//! ~330s broker-inner / 360s wire budget (wire.rs `EXT_MODELS_CALL_TIMEOUT`). We rebuild
//! multi-turn ourselves: `office_summary` (rolling) + the newest
//! transcript turns + the output contract. When the assembled prompt would cross
//! [`FOLD_THRESHOLD`], the kernel first issues a summarize invoke ([`build_fold`]) that
//! compresses the oldest half into `office_summary` ([`apply_fold`]), then re-issues the
//! persona invoke. [`build_invoke`] additionally HARD-truncates at [`HARD_PROMPT_CAP`]
//! (dropping oldest turns first, then the summary) so a pathological transcript can
//! never produce a `prompt exceeds 32KB` host error.

use serde::Deserialize;
use std::path::{Path, PathBuf};

use crate::domain::{ChatAuthor, ChatMsg, Epic, EpicId, Project, ProjectPhase, Sprint, SprintLine, SprintStatus, Story, StoryId, Task, TaskId, TaskState};
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
    /// The SDLC intake TRIAGE classifier (feature: sdlc-triage). Fired ONCE, additionally, at the
    /// first message of a fresh brief, on the `safeguard_role` (a gate-family classification, NOT a
    /// doc-drafting invoke — so never `drafter_model`). Returns a strictly-parseable `SDLC-TRIAGE`
    /// block; the kernel parses it defensively ([`crate::report::parse_triage`]) and routes the
    /// project to the `project` / `enhancement` / `patch` track, defaulting to `project` on any
    /// error or unparseable result.
    Triage,
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
    /// The COMBINED Technical + Clean-build Requirement Document authoring call (design-speedup
    /// item 3), issued once the PRD gate has cleared AND research has settled (the research join).
    /// One invoke authors BOTH docs: the reply must carry a ```trd fenced block AND a ```crd fenced
    /// block. Both are captured; a capture-miss nudge fires (shared budget) if EITHER is missing. A
    /// captured TRD kicks off the early breakdown and runs the single combined TRD+CRD safeguard
    /// gate; any `Err` still proceeds so Drafting never wedges. Supersedes the old separate `Trd`
    /// and `Crd` invokes.
    TrdCrd,
    /// The safeguard no-assume gate over the PRD (ARCHITECTURE.md 6.2c / design-speedup one-shot
    /// gate). Runs on the `safeguard_role`. It is the ENUMERATE pass; in `assumption_mode == "auto"`
    /// it ALSO resolves the non-critical items inline and re-emits the revised ```prd (compressed
    /// gate). When `research_mode == "auto"` it additionally answers the well-known boolean that
    /// decides whether to run research. `clean` -> proceed (research join); `assumptions` -> resolve
    /// / verify / freeze-critical per mode; `Err` -> FAIL-OPEN. A distinct variant per doc-set keeps
    /// the type `Copy`.
    AssumeCheckPrd,
    /// The safeguard no-assume gate over the COMBINED TRD+CRD doc-set (design-speedup one-shot gate,
    /// item 3+5). Same shape as `AssumeCheckPrd` but over both docs at once; a clean/settled verdict
    /// proceeds to the breakdown join. Supersedes the old separate `AssumeCheckTrd`/`AssumeCheckCrd`.
    AssumeCheckTrdCrd,
    /// The batch assumption-RESOLUTION invoke, emitted only in `assumption_mode == "ask"` for the
    /// non-critical remainder after the enumerate pass surfaced (and froze on) any critical items
    /// (design-speedup one-shot gate; in `auto` mode the resolution is folded into the enumerate
    /// invoke instead). The office decides each `[auto]` item itself, revises the doc-set, and
    /// re-emits the COMPLETE doc(s) in their fence(s). The kernel updates the doc(s) then runs the
    /// single VERIFY pass. The doc-set is recovered from state (`newest_gated_doc`), so one Copy
    /// variant suffices. An `Err`/missing fence PROCEEDS (never wedges).
    AssumeResolve,
    /// The FINAL verify pass of the one-shot gate (design-speedup item 5). Runs on the
    /// `safeguard_role` over the revised doc-set. It may ONLY confirm clean OR list REMAINING
    /// material assumptions to DISCLOSE — it never triggers another resolve round. Newly-flagged
    /// items are recorded as disclosed (`self_resolved_assumptions`) and the gate clears anyway. The
    /// doc-set is recovered from state (`newest_gated_doc`), so one Copy variant suffices.
    AssumeVerify,
    /// The per-sprint review CEREMONY synthesis (feature: sprints). Fired EXACTLY ONCE when a
    /// sprint's tasks all settle: the transcript (worker/reviewer/researcher lines) is assembled
    /// PROGRAMMATICALLY from real task data first, and this single invoke only adds the PM's closing
    /// line + any adjustments to the NEXT sprint (parsed defensively — unparseable => carry-overs
    /// only). Runs on the `office_role` like a persona reply, but is NOT a doc-drafting invoke, so
    /// it is emitted via `emit_invoke` and does NOT take the `drafter_model` override (which stays
    /// scoped to actual PRD/TRD/CRD authoring). The result routes to `finish_sprint_review`.
    SprintReview,
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

/// The persona's powerlessness clause (safeguard hardening, ARCHITECTURE.md 6.2c): the front
/// office is a PLANNER, not an executor — no prose it emits moves the production line, and no
/// worker can hear it. Work begins ONLY when the system captures a fenced doc AND the human
/// authorizes a delivery path. Appended to every doc-authoring contract so the persona never
/// roleplays dispatching / greenlighting / addressing workers (live-test 2026-07-15: a prose
/// "workers — you're greenlit, @worker1 go" left the project wedged in Drafting, since no fence
/// was emitted and the stopped gate never re-ran).
pub const POWERLESSNESS_CLAUSE: &str = "\nYou cannot start work, dispatch, greenlight, or address \
workers — no worker can hear you and NOTHING happens from prose. The ONLY way work begins: the \
system captures your fenced docs, then the human authorizes with a delivery path. Never roleplay \
execution or progress — if you want the line to move, emit the fenced doc and ask the human to \
authorize.\n";

/// The disclose-and-re-emit clause (safeguard hardening, ARCHITECTURE.md 6.2c): minor
/// implementation choices are DISCLOSED under a 'Proposed defaults (applied unless you object)'
/// heading rather than buried in the body (the safeguard never flags disclosed defaults), and
/// once the user approves the doc or delegates a choice, the persona RE-EMITS the COMPLETE doc in
/// its fence with 'Delegated decision:' annotations — that re-emitted fence is what re-runs the
/// gate and advances the pipeline (a belt to the kernel's re-check-on-reply brace).
pub const DISCLOSE_REEMIT_CLAUSE: &str = "\nMinor implementation choices you make (reasonable \
defaults) belong under a 'Proposed defaults (applied unless you object)' heading — disclosed, \
never hidden in the doc body. After the user approves the doc or delegates a choice to you, \
RE-EMIT the COMPLETE updated document inside its fence, annotating each now-settled item as \
'Delegated decision: ...' — that re-emitted fence is what advances the pipeline.\n";

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

/// The ENHANCEMENT-track persona contract (feature: sdlc-triage): the intake classified this brief
/// as a scoped change to an EXISTING deliverable, so the office drafts ONE change-brief (```change),
/// NOT the full PRD/TRD/CRD trio. The change-brief takes the PRD doc-slot, so the gate/JOIN
/// machinery reuses unchanged. Sections are fixed: Current behavior / Desired behavior / Acceptance
/// criteria.
const CHANGE_BRIEF_CONTRACT: &str = "\nRespond as the Workflow front office for a scoped ENHANCEMENT \
to an EXISTING deliverable: negotiate the change, answer clearly, and drive toward ONE change-brief. \
Be concise and decisive — this is a change, not a new build, so keep it tight.\n\
When (and only when) the change is agreed, emit the COMPLETE change-brief as markdown inside a \
fenced block that starts with ```change and ends with ``` — that exact fence is how the system \
captures it and starts the line; a change-brief outside that fence does not count and nothing will \
happen. Use EXACTLY these sections: 'Current behavior', 'Desired behavior', 'Acceptance criteria'.\n\
Do NOT assume anything the user did not state. Every choice that is not user-stated, \
research-grounded, or explicitly delegated ('you decide' / 'up to you') belongs under an \
'Open questions' section, never in the change-brief body. Record delegated choices as 'Delegated \
decision: ...'.\n";

/// The PATCH-track persona contract (feature: sdlc-triage): a tiny, well-specified fix handled as a
/// SINGLE task with NO documents — the system builds the task directly from the brief. The office
/// only converses/confirms; it never drafts a doc, so this contract emits no fence.
const PATCH_CONTRACT: &str = "\nRespond as the Workflow front office for a small PATCH: acknowledge \
the fix, keep it to a sentence or two, and confirm scope. This is a tiny change handled as ONE task \
built directly from the brief — you do NOT draft any document and there is no fence to emit.\n";

/// Capture the LAST ` ```<tag> ` fenced block from a persona reply, if any — the generalized
/// engine behind PRD (6.2), TRD (6.2b), and CRD (6.2c) capture. The fence is the explicit capture
/// contract given to the persona (PRD: [`PERSONA_CONTRACT`]; TRD: [`build_trd_prompt`]; CRD:
/// [`build_crd_prompt`]); free-text 'here is the doc' prose is deliberately ignored. `tag` is the
/// language hint after the opening backticks (`"prd"` / `"trd"` / `"crd"`).
///
/// Line-oriented and forgiving of real model output (fence hardening):
///  - The tag is matched CASE-INSENSITIVELY and only as the FIRST token of the fence line's info
///    string, so ` ```PRD `, ` ```prd `, and ` ```prd (final draft) ` all open a "prd" block while
///    ` ```prdx ` / ` ```rust ` do not.
///  - The body terminates at the LAST lone closing ``` (a line that is exactly ``` after trimming),
///    not the first — so an embedded ` ```rust … ``` ` code block inside a document survives instead
///    of truncating it. If there is no lone closing fence after the opening one, the remainder is
///    taken (the unterminated-fence fallback).
///
/// An empty captured body yields `None`. When several ` ```<tag> ` blocks are present the LAST
/// opening fence wins (a re-emitted doc supersedes an earlier draft).
pub fn extract_fenced(reply: &str, tag: &str) -> Option<String> {
    let lines: Vec<&str> = reply.lines().collect();
    // LAST opening fence for this tag wins (a re-emitted doc supersedes an earlier draft).
    let open_idx = lines.iter().rposition(|l| is_open_fence(l, tag))?;
    // Greedy close: the LAST lone ``` AFTER the opening fence, so an embedded ```code``` block
    // inside the doc never truncates it. No lone close -> unterminated, take the remainder.
    let body: &[&str] = match lines[open_idx + 1..].iter().rposition(|l| is_lone_fence(l)) {
        Some(rel) => &lines[open_idx + 1..open_idx + 1 + rel],
        None => &lines[open_idx + 1..],
    };
    let doc = body.join("\n");
    let doc = doc.trim();
    if doc.is_empty() {
        None
    } else {
        Some(doc.to_string())
    }
}

/// Whether `line` opens a ` ```<tag> ` fenced block: after the three backticks, the info string's
/// FIRST whitespace-delimited token must equal `tag` case-insensitively. Tolerates leading
/// whitespace, a mixed-case tag, and any trailing text after the tag; a bare ` ``` ` (no info
/// string) is a closing/lone fence, never an opening tag fence.
fn is_open_fence(line: &str, tag: &str) -> bool {
    match line.trim_start().strip_prefix("```") {
        Some(rest) => rest.split_whitespace().next().is_some_and(|t| t.eq_ignore_ascii_case(tag)),
        None => false,
    }
}

/// Whether `line` is a lone closing fence — exactly ` ``` ` once surrounding whitespace is trimmed
/// (so it never matches an opening ` ```rust ` info line).
fn is_lone_fence(line: &str) -> bool {
    line.trim() == "```"
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

/// The explicit trailing fence reminder appended LAST to every doc-drafting / revision prompt
/// (design-speedup item 1: fence hardening). Recency compliance — a slow model that narrated the
/// doc but forgot the fence wastes a whole round on a capture miss (each miss = a capture nudge), so
/// the very last thing the model reads is the EXACT required wrapper. Per-doc `tags` (`["prd"]`, or
/// `["trd", "crd"]` for the combined authoring invoke). Kept OUT of `extract_fenced` (the capture
/// engine is untouched) — this only steers the model toward emitting what that engine already reads.
pub fn fence_reminder(tags: &[&str]) -> String {
    match tags {
        [tag] => format!(
            "\nReminder: your reply MUST END with the document — nothing after its closing fence. \
             Wrap it EXACTLY as:\n```{tag}\n...\n```\n",
        ),
        many => {
            let mut s = String::from(
                "\nReminder: your reply MUST END with EVERY document below — nothing after the final \
                 closing fence. Wrap each EXACTLY in its OWN fenced block:\n",
            );
            for t in many {
                s.push_str(&format!("```{t}\n...\n```\n"));
            }
            s
        }
    }
}

/// Capture the TRD and CRD from a COMBINED reply that carries BOTH a ```trd and a ```crd block
/// (design-speedup item 3). [`extract_fenced`] is deliberately greedy (its close is the LAST lone
/// ``` so an embedded code block survives), which for two ADJACENT doc fences would let the FIRST
/// swallow the second. This helper first splits the reply at the boundary between the two opening
/// fences (in either order), then runs [`extract_fenced`] on each half — so each doc is captured on
/// its own while embedded code blocks WITHIN a doc still survive. `extract_fenced` itself is left
/// untouched. Returns `(trd, crd)`; either is `None` when its fence is absent.
pub fn extract_trd_crd(reply: &str) -> (Option<String>, Option<String>) {
    let lines: Vec<&str> = reply.lines().collect();
    let trd_open = lines.iter().rposition(|l| open_fence_is(l, "trd"));
    let crd_open = lines.iter().rposition(|l| open_fence_is(l, "crd"));
    match (trd_open, crd_open) {
        (Some(t), Some(c)) => {
            // Split at the later opening fence so neither half contains the other's fence.
            let (first_tag, first_range, second_tag, second_range) = if t < c {
                ("trd", 0..c, "crd", c..lines.len())
            } else {
                ("crd", 0..t, "trd", t..lines.len())
            };
            let first = extract_fenced(&lines[first_range].join("\n"), first_tag);
            let second = extract_fenced(&lines[second_range].join("\n"), second_tag);
            if first_tag == "trd" {
                (first, second)
            } else {
                (second, first)
            }
        }
        // Only one (or neither) fence present: greedy extraction is already correct.
        _ => (extract_fenced(reply, "trd"), extract_fenced(reply, "crd")),
    }
}

/// Whether `line` opens a ` ```<tag> ` fenced block (case-insensitive first info token). Thin
/// re-export of the private [`is_open_fence`] shape for [`extract_trd_crd`]'s boundary scan.
fn open_fence_is(line: &str, tag: &str) -> bool {
    is_open_fence(line, tag)
}

/// Pick the persona's doc-authoring contract + fence tag(s) for the project's SDLC track
/// (feature: sdlc-triage). `"enhancement"` drafts a ```change change-brief; `"patch"` drafts NO doc
/// (the board is built programmatically at triage-resolve); every other value — including the
/// `"project"` default — is the unchanged PRD contract, so the project golden path is byte-identical.
fn track_contract(track: &str) -> (&'static str, &'static [&'static str]) {
    match track {
        "enhancement" => (CHANGE_BRIEF_CONTRACT, &["change"]),
        "patch" => (PATCH_CONTRACT, &[]),
        _ => (PERSONA_CONTRACT, &["prd"]),
    }
}

fn assemble(summary: &str, turns: &[&ChatMsg], new_user_msg: &str, track: &str) -> String {
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
    let (contract, tags) = track_contract(track);
    prompt.push_str(contract);
    prompt.push_str(POWERLESSNESS_CLAUSE);
    prompt.push_str(DISCLOSE_REEMIT_CLAUSE);
    // Fence hardening (item 1): the LAST line the model reads is the exact fence wrapper for this
    // track's doc (```prd for project, ```change for enhancement). Patch drafts no doc — no reminder.
    if !tags.is_empty() {
        prompt.push_str(&fence_reminder(tags));
    }
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
        let prompt = assemble(&p.office_summary, &turns, new_user_msg, &p.track);
        if prompt.len() <= HARD_PROMPT_CAP {
            return (system, prompt);
        }
        if turns.is_empty() {
            // Even with no turns the prompt overflows: the summary is pathological.
            // Truncate it hard and re-assemble; guard the final size unconditionally.
            let summary = truncate_bytes(&p.office_summary, HARD_PROMPT_CAP / 2);
            let prompt = assemble(&summary, &[], new_user_msg, &p.track);
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
    assemble(&p.office_summary, &turns, new_user_msg, &p.track).len() > FOLD_THRESHOLD
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
// SDLC intake triage (feature: sdlc-triage)
// ---------------------------------------------------------------------------

/// Build the intake-TRIAGE classifier `(system, prompt)` for `models.invoke` (feature: sdlc-triage).
/// Runs on the `safeguard_role` — a lightweight gate-family classification, NOT a doc-drafting
/// invoke. It reads the user's `brief` and returns a strictly-parseable `SDLC-TRIAGE` block the
/// kernel parses defensively ([`crate::report::parse_triage`]); an unparseable / errored result
/// defaults to the full `project` ceremony. Pure + byte-bounded like every other prompt builder.
pub fn build_triage_prompt(p: &Project, brief: &str) -> (String, String) {
    let system = prompts::office_system(&board_digest(p));
    let mut prompt = String::new();
    prompt.push_str(
        "You are the Workflow intake TRIAGE classifier. Read the user's brief and classify the work \
into EXACTLY ONE delivery track. Be decisive but CONSERVATIVE: when unsure, choose 'project' (the \
full process). You do NOT design, plan, or draft anything — you ONLY classify.\n\n",
    );
    prompt.push_str("USER BRIEF:\n");
    prompt.push_str(&truncate_bytes(brief, HARD_PROMPT_CAP / 2));
    prompt.push_str(
        "\n\nThe three tracks:\n\
- project: a NEW system, feature, or non-trivial build that warrants the full PRD/TRD/CRD ceremony. \
This is the DEFAULT — choose it whenever the scope is unclear or larger than a single change.\n\
- enhancement: a SCOPED change or addition to an EXISTING deliverable — small enough for one \
change-brief and a handful of tasks.\n\
- patch: a TINY, well-specified fix or tweak that needs no documents and is one obvious task.\n\
\n\
Output ONLY this block, nothing else — no preamble, no code fence:\n\
SDLC-TRIAGE\n\
track: project | enhancement | patch\n\
rationale: <one line explaining the choice>\n\
existing: yes | no   (does the brief target an EXISTING delivery? only meaningful for enhancement/patch)\n",
    );
    (system, truncate_bytes(&prompt, HARD_PROMPT_CAP))
}

/// Marks the boundary between the accumulated sprint-learnings prefix (newest addition first) and
/// the ORIGINAL stack-research tail within `research_notes` (review finding, MINOR: eviction floor).
/// Only ever inserted by [`append_research_learnings`]; reads as a natural section header in the
/// rendered notes, and lets that function recover the split on every subsequent call.
const ORIGINAL_RESEARCH_HEADER: &str = "\n\n## Original Stack Research\n\n";

/// Append sprint-review learnings to `existing` research notes, keeping the NEWEST content within
/// [`RESEARCH_NOTES_CAP`] (feature: sprints item 4).
///
/// Review finding (MINOR, eviction floor): the original implementation PREPENDED the addition and
/// tail-truncated the combined text — correct for one call, but across enough verbose sprints the
/// accumulated learnings prefix alone grows past the cap, and tail-truncation then eats the ORIGINAL
/// stack research (at the tail) entirely. Fixed by reserving a floor: `existing` is split at
/// [`ORIGINAL_RESEARCH_HEADER`] into the accumulated learnings prefix and the original research tail
/// (the very first call — before any header exists — treats the WHOLE of `existing` as that original
/// tail). The new prefix (this addition + the old prefix) is capped at `RESEARCH_NOTES_CAP / 2`
/// *before* recombining with the tail, so the newest learnings can never crowd the original research
/// out of the notes; the final recombination is still capped to `RESEARCH_NOTES_CAP` overall, which
/// only ever trims the END of the original tail, never erasing its start. An empty addition is a
/// no-op.
pub fn append_research_learnings(existing: &str, addition: &str) -> String {
    if addition.trim().is_empty() {
        return existing.to_string();
    }

    let (learnings_prefix, original_tail) = match existing.find(ORIGINAL_RESEARCH_HEADER) {
        Some(idx) => (
            existing[..idx].to_string(),
            existing[idx + ORIGINAL_RESEARCH_HEADER.len()..].to_string(),
        ),
        None => (String::new(), existing.to_string()),
    };

    let mut new_prefix = if learnings_prefix.trim().is_empty() {
        addition.to_string()
    } else {
        format!("{addition}\n\n{learnings_prefix}")
    };

    // Floor: the learnings prefix alone may never eat more than half the cap, so the original
    // research tail always keeps at least the other half.
    let floor = RESEARCH_NOTES_CAP / 2;
    if new_prefix.len() > floor {
        new_prefix = truncate_bytes(&new_prefix, floor);
    }

    if original_tail.trim().is_empty() {
        return new_prefix;
    }

    let combined = format!("{new_prefix}{ORIGINAL_RESEARCH_HEADER}{original_tail}");
    truncate_bytes(&combined, RESEARCH_NOTES_CAP)
}

/// Build the sprint-review CEREMONY synthesis `(system, prompt)` for `models.invoke` (feature:
/// sprints). The transcript participant lines (`transcript`) were assembled PROGRAMMATICALLY from
/// real task data — this single invoke only asks the PM to (1) write a short sprint summary and (2)
/// optionally adjust the NEXT sprint's task list. Runs on the `office_role` (see
/// [`InvokePurpose::SprintReview`]). Pure + byte-bounded like every other builder. `next` is the
/// next sprint's `(goal, [(task-id, title)])` the PM may edit — `None` when this is the last sprint
/// (no adjustments possible, summary only).
pub fn build_sprint_review_prompt(
    p: &Project,
    sprint_goal: &str,
    transcript: &[SprintLine],
    carry_over_titles: &[String],
    next: Option<(&str, &[(String, String)])>,
) -> (String, String) {
    let system = prompts::office_system(&board_digest(p));
    let mut prompt = String::new();
    prompt.push_str(&format!(
        "You are the Workflow front office (delivery manager) running the review for the sprint whose \
goal was:\n{}\n\n",
        truncate_bytes(sprint_goal.trim(), 400)
    ));
    prompt.push_str("THE ROOM ALREADY REPORTED (real data — do not contradict it):\n");
    let mut room = String::new();
    for line in transcript {
        room.push_str(&format!("{}: {}\n", line.speaker, line.line));
    }
    prompt.push_str(&truncate_bytes(&room, HARD_PROMPT_CAP / 3));

    if carry_over_titles.is_empty() {
        prompt.push_str("\nCARRY-OVER: none — every task in this sprint reached done.\n");
    } else {
        prompt.push_str("\nCARRY-OVER (tasks NOT completed this sprint — they move to the next sprint):\n");
        let list: String = carry_over_titles.iter().map(|t| format!("- {t}\n")).collect();
        prompt.push_str(&truncate_bytes(&list, HARD_PROMPT_CAP / 6));
    }

    match next {
        Some((next_goal, tasks)) => {
            prompt.push_str(&format!(
                "\nNEXT SPRINT — goal: {}\nits currently-planned tasks (you MAY adjust them):\n",
                truncate_bytes(next_goal.trim(), 300)
            ));
            let list: String = tasks
                .iter()
                .map(|(id, title)| format!("- [{}] {}\n", id, truncate_bytes(title.trim(), 120)))
                .collect();
            prompt.push_str(&truncate_bytes(&list, HARD_PROMPT_CAP / 4));
            prompt.push_str(
                "\nSynthesize a SHORT sprint summary, then adjust the NEXT sprint's task list ONLY if \
what you learned this sprint warrants it. Output ONLY this block, nothing else:\n\
SPRINT-REVIEW\n\
summary: <2-4 lines: what shipped, and what carried over and why>\n\
adjustments:\n\
- drop <exact task id above to remove>\n\
- add <new task title> | <one-line description>\n\
- modify <exact task id above> | <new one-line description>\n\
(omit the '- ' lines entirely when nothing should change — carry-overs move over either way)\n",
            );
        }
        None => {
            prompt.push_str(
                "\nThis was the LAST sprint — there is no next sprint to adjust. Output ONLY this \
block, nothing else:\n\
SPRINT-REVIEW\n\
summary: <2-4 lines: what shipped across the project>\n",
            );
        }
    }
    (system, truncate_bytes(&prompt, HARD_PROMPT_CAP))
}

/// A minimal, gradeable clean-build CRD for the ENHANCEMENT track (feature: sdlc-triage). An
/// enhancement inherits the existing delivery's requirements, so only clean-build HYGIENE is graded
/// here — the completion auditor still needs a checklist + a rubric summing to 100. Generated
/// PROGRAMMATICALLY (no invoke round) when the enhancement has no prior CRD to inherit.
pub fn minimal_hygiene_crd() -> String {
    "# Clean-build Requirement Document (minimal — enhancement track)\n\n\
This enhancement builds on an existing delivery; its functional requirements live in the \
change-brief. Only clean-build HYGIENE of the integrated tree is graded here.\n\n\
## Checklist\n\
- No dependency or build artifacts committed (node_modules/, vendor/, target/, dist/, build/, \
.vite/, .next/, __pycache__/, *.pyc, coverage/).\n\
- No temp / editor / runtime droppings (*.tmp, *.bak, *.orig, *.swp, *.log, .DS_Store, SQLite \
WAL/journal sidecars).\n\
- No commented-out dead code, stray debug prints, or unwired/orphan files.\n\
- The change builds and lints cleanly.\n\
- A README is present at the delivery root.\n\n\
## Grading rubric (points sum to 100)\n\
- Clean tree, no build/dependency artifacts or trash: 40\n\
- Builds and lints cleanly: 40\n\
- No unwired/orphan files; README present: 20\n"
        .to_string()
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
    // Sprint grouping (feature: sprints), project track only — the enhancement track is a scoped
    // change that stays a single implicit sprint, and compact mode drops it to keep the ask tiny.
    // Absent/partial/garbage `sprints` is safe: the validator falls back to one all-tasks sprint.
    if p.track != "enhancement" && !compact {
        prompt.push_str(
            "\nAlso group the tasks into ordered SPRINTS — each a shippable increment with a \
one-line goal — by adding a top-level \"sprints\" array alongside \"epics\":\n\
\"sprints\":[{\"goal\":\"..\",\"tasks\":[\"task-slug\",\"task-slug\"]}]\n\
Every task slug appears in EXACTLY ONE sprint; order sprints so a task's blocked_by prerequisites \
land in the same sprint or an earlier one; a small plan may be a SINGLE sprint. Use the task slugs \
exactly as above.\n",
        );
    }
    // SDLC enhancement track (feature: sdlc-triage): a scoped change is small by definition — cap the
    // plan to 1-3 tasks in one epic. The kernel escalates to the full project track if the model
    // returns more than 3 anyway (wider scope than a change).
    if p.track == "enhancement" {
        prompt.push_str(
            "\nSMALL SCOPE (this is a scoped ENHANCEMENT, not a new build): produce AT MOST 3 tasks \
total (ideally 1-3), in exactly one epic and one story. If the change genuinely needs more than 3 \
tasks, it is wider than an enhancement — still cap at 3 here.\n",
        );
    }
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

/// Build the COMBINED TRD+CRD authoring `(system, prompt)` for `models.invoke` (design-speedup
/// item 3). ONE invoke authors BOTH docs: the reply must carry a ```trd fenced block AND a ```crd
/// fenced block. Built from the PRD (+ research findings when present), byte-bounded, and — item 1
/// — ENDS with the explicit both-fences reminder. Supersedes `build_trd_prompt`/`build_crd_prompt`.
pub fn build_trdcrd_prompt(p: &Project) -> (String, String) {
    let system = prompts::office_system(&board_digest(p));
    let mut prompt = String::new();
    prompt.push_str(
        "Draft BOTH the Technical Requirements Document (TRD) and the Clean-build Requirement \
Document (CRD) for the PRD below, in ONE reply.\n\n",
    );
    prompt.push_str("PRD:\n");
    prompt.push_str(&truncate_bytes(&p.prd_markdown, HARD_PROMPT_CAP / 3));
    if !p.research_notes.trim().is_empty() {
        prompt.push_str("\n\nRESEARCH FINDINGS (web-researched stack notes — weigh these):\n");
        prompt.push_str(&truncate_bytes(&p.research_notes, HARD_PROMPT_CAP / 4));
    }
    prompt.push_str(
        "\n\nEmit TWO fenced blocks, in this order:\n\
1) The COMPLETE Technical Requirements Document inside a block that starts with ```trd and ends \
with ``` — cover, as sections: technology stack with SPECIFIC current stable versions, \
architecture, data model, API surface, testing strategy, deployment, and constraints. This \
document drives the epic/story/task breakdown.\n\
2) The COMPLETE Clean-build Requirement Document inside a block that starts with ```crd and ends \
with ``` — a concrete, gradeable acceptance checklist a read-only auditor grades the delivered \
tree against. Cover: expected file-tree shape (what must and must NOT be present); no unwired \
files; no trash (temp/`.bak`/dead deps/commented-out code/debug prints); build + lint pass; a \
README present; any project-specific correctness gates. End it with a 'Grading rubric' section \
whose point weights SUM TO EXACTLY 100.\n\
Be concrete and decisive; the CRD must match the TRD's choices.\n",
    );
    prompt.push_str(NO_ASSUME_CLAUSE);
    prompt.push_str(POWERLESSNESS_CLAUSE);
    prompt.push_str(DISCLOSE_REEMIT_CLAUSE);
    // Fence hardening (item 1): the last lines the model reads are the exact ```trd + ```crd wrappers.
    prompt.push_str(&fence_reminder(&["trd", "crd"]));
    (system, truncate_bytes(&prompt, HARD_PROMPT_CAP))
}

/// The combined TRD+CRD doc-set body (design-speedup item 3), labeled per doc, used as the
/// `doc_body` for the single TRD+CRD safeguard gate (check / resolve / verify). Missing docs render
/// as a placeholder so a partially-captured set still gates cleanly.
pub fn trdcrd_body(p: &Project) -> String {
    let trd = if p.trd_markdown.trim().is_empty() { "(not drafted)" } else { p.trd_markdown.as_str() };
    let crd = if p.crd_markdown.trim().is_empty() { "(not drafted)" } else { p.crd_markdown.as_str() };
    format!("## Technical Requirements Document (TRD)\n{trd}\n\n## Clean-build Requirement Document (CRD)\n{crd}")
}

/// The shared prompt preamble for every safeguard gate pass (design-speedup one-shot gate): the
/// user's OWN turns (ground truth), the research notes, the doc-set under review, and the material-
/// vs-micro-detail flag rules. The distinct passes (enumerate / verify) append their own output
/// contracts. Only the user's own turns count as ground truth — the office's prior replies are
/// exactly the assumptions we are trying to catch.
fn assume_context(p: &Project, doc_label: &str, doc_body: &str) -> String {
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
    prompt.push_str(&format!("\n{doc_label} UNDER REVIEW:\n"));
    prompt.push_str(&truncate_bytes(doc_body, HARD_PROMPT_CAP / 3));
    prompt.push_str(
        "\n\nFlag ONLY MATERIAL assumptions — a choice is material only if it changes cost, scope, \
or the deliverable:\n\
- technology / stack / framework / language / database choices the user did not state\n\
- scope added or removed (features, integrations, platforms) beyond what was asked\n\
- data persistence, storage, or external services / third-party APIs introduced\n\
- security or auth posture (who can access what, how secrets/credentials are handled)\n\
- anything else that is clearly cost- or deliverable-shaping\n\
Do NOT flag implementation micro-details — these are the author's job and are NEVER assumptions: \
input-validation specifics, display / formatting choices, sort ordering, folder or file layout, \
UI transitions, naming, trimming input, character counts, or other reasonable defaults.\n\
NEVER flag anything the document discloses under a heading like 'Proposed defaults', 'Delegated \
decisions', or 'Open questions' — those are already surfaced, not hidden assumptions.\n\
If the user's statements contain ANY delegation ('you decide' / 'up to you' / 'your call' / \
'approved' / 'proceed'), the verdict is clean — the user handed the office the pen.\n",
    );
    prompt
}

/// The `[critical]`/`[auto]` criticality-tagging instructions, shared by the enumerate passes.
const ASSUME_TAG_RULES: &str = "\nTAG EACH flagged item with its criticality — the office will \
decide the rest ITSELF, so reserve [critical] for the NARROW set of choices that genuinely need a \
human:\n\
- [critical] ONLY IF the choice: spends real money; requires accounts, credentials, or secrets; \
modifies or deletes EXISTING user data or systems; picks a deployment target going live; or \
creates legal-exposure content. These need a human before anything happens.\n\
- [auto] for EVERYTHING else — stack / library / framework / language / database choice, data \
format, project structure, scope details, UX details, and every other reasonable design decision. \
The office is trusted to decide these; do NOT mark them critical.\n\
When unsure, tag [auto], not [critical].\n";

/// The `well-known:` addendum (design-speedup item 4, `research_mode == "auto"`): the ONE extra
/// boolean the PRD enumerate pass also answers so the kernel can decide whether to run research.
const WELL_KNOWN_ADDENDUM: &str = "\nSEPARATELY, judge the technology involved: is the ENTIRE stack \
this document implies mainstream and well-known (so current versions / best practices need no web \
research)? Add EXACTLY one line:\n\
well-known: yes | no\n";

/// Build the ENUMERATE pass of the one-shot safeguard gate `(system, prompt)` (design-speedup item
/// 5 + amendment A). `tags` are the fence tag(s) of the doc-set (`["prd"]` or `["trd", "crd"]`).
/// When `resolve_inline` (`assumption_mode == "auto"`) the safeguard ALSO decides the non-critical
/// items itself and re-emits the revised document(s) in the SAME reply — collapsing enumerate +
/// batch-resolve into one invoke. When `ask_wellknown` (PRD + `research_mode == "auto"`) it also
/// answers the well-known boolean. Emitted by the kernel on the `safeguard_role`.
pub fn build_assume_check_prompt(
    p: &Project,
    doc_label: &str,
    doc_body: &str,
    tags: &[&str],
    resolve_inline: bool,
    ask_wellknown: bool,
) -> (String, String) {
    let mut system = String::from(
        "You are a requirements safeguard with a HIGH bar. Your ONE job is to catch MATERIAL \
ungrounded assumptions: decisions that shape cost, scope, or the deliverable that the user never \
stated, research never established, and the user never delegated. You do NOT nitpick implementation \
details — you flag only choices that would change WHAT gets built or how much it costs. Be precise \
and conservative: when in doubt, do not flag.",
    );
    if resolve_inline {
        system.push_str(
            " In addition you are TRUSTED to DECIDE every non-critical ([auto]) assumption yourself \
— stack, libraries, formats, structure, UX — and revise the document(s) to bake those decisions in, \
leaving [critical] items UNRESOLVED and open for the human.",
        );
    }

    let mut prompt = assume_context(p, doc_label, doc_body);
    prompt.push_str(ASSUME_TAG_RULES);
    prompt.push_str(
        "\nFIRST output this block:\n\
ASSUME-CHECK\n\
verdict: clean | assumptions\n\
- [critical] <one MATERIAL ungrounded assumption per line>\n\
- [auto] <one MATERIAL ungrounded assumption per line>\n\
(omit the '- ' lines entirely when verdict is clean; an untagged item is treated as [auto])\n",
    );
    if ask_wellknown {
        prompt.push_str(WELL_KNOWN_ADDENDUM);
    }
    if resolve_inline {
        prompt.push_str(
            "\nTHEN, for every [auto] item (leave [critical] items untouched and open), DECIDE it \
yourself with best judgment and the research notes, revise the document(s) so each decision is \
baked in under a 'Delegated decisions (auto)' heading (one-line rationale each), and re-emit the \
COMPLETE revised document(s) — each in its OWN fenced block:\n",
        );
        for t in tags {
            prompt.push_str(&format!("```{t}\n...revised {}...\n```\n", t.to_uppercase()));
        }
        prompt.push_str(
            "Put the ASSUME-CHECK block (and any well-known line) FIRST, then the fenced \
document(s). When the verdict is clean, emit no fenced document.\n",
        );
    }
    (system, truncate_bytes(&prompt, HARD_PROMPT_CAP))
}

/// Build the FINAL VERIFY pass of the one-shot safeguard gate `(system, prompt)` (design-speedup
/// item 5). The doc-set has already been revised; this pass may ONLY confirm it is clean OR LIST any
/// material assumptions that REMAIN (for disclosure) — it NEVER rewrites or resolves, so the gate
/// can never loop. Emitted on the `safeguard_role`; the kernel records any listed items as disclosed.
pub fn build_assume_verify_prompt(p: &Project, doc_label: &str, doc_body: &str) -> (String, String) {
    let system = "You are a requirements safeguard doing a FINAL verification pass. The document(s) \
below have ALREADY been revised to resolve open assumptions. Your ONLY job now is to VERIFY: either \
confirm they are clean, or LIST any MATERIAL ungrounded assumptions that still REMAIN. You do NOT \
rewrite, resolve, or re-open anything — you only report what is left. Be precise and conservative."
        .to_string();

    let mut prompt = assume_context(p, doc_label, doc_body);
    prompt.push_str(
        "\nOutput ONLY this block, nothing else — no fenced document, no rewrite:\n\
ASSUME-CHECK\n\
verdict: clean | assumptions\n\
- <one MATERIAL ungrounded assumption that REMAINS, per line>\n\
(omit the '- ' lines entirely when verdict is clean)\n",
    );
    (system, truncate_bytes(&prompt, HARD_PROMPT_CAP))
}

/// Build the ASK-mode batch assumption-resolution `(system, prompt)` for `models.invoke` (design-
/// speedup one-shot gate). Emitted only in `assumption_mode == "ask"` for the non-critical remainder
/// after the enumerate pass surfaced (and froze on) any critical items. The office decides each
/// `[auto]` item itself and re-emits the COMPLETE revised document(s), each in its OWN fence
/// (`tags`: `["prd"]` or `["trd", "crd"]`). Pure + byte-bounded.
pub fn build_assume_resolve_prompt(
    p: &Project,
    doc_label: &str,
    doc_body: &str,
    auto_items: &[String],
    tags: &[&str],
) -> (String, String) {
    let system = format!(
        "You are the Workflow front office resolving your own open assumptions. You are TRUSTED to \
decide reasonable design choices yourself — stack, libraries, formats, structure, UX details. \
Decide each assumption below with best judgment and the research notes, then revise the {doc_label} \
to reflect those decisions. Be decisive; do NOT punt back to the human on non-critical calls.",
    );

    let mut prompt = String::new();
    prompt.push_str(&format!("{doc_label} UNDER REVISION:\n"));
    prompt.push_str(&truncate_bytes(doc_body, HARD_PROMPT_CAP / 3));
    prompt.push_str("\n\nASSUMPTIONS TO DECIDE YOURSELF (each is non-critical — settle it):\n");
    if auto_items.is_empty() {
        prompt.push_str("(none listed — re-emit the document(s) unchanged)\n");
    } else {
        let list: String = auto_items.iter().map(|a| format!("- {a}\n")).collect();
        prompt.push_str(&truncate_bytes(&list, HARD_PROMPT_CAP / 4));
    }
    if !p.research_notes.trim().is_empty() {
        prompt.push_str("\nRESEARCH FINDINGS (lean on these where relevant):\n");
        prompt.push_str(&truncate_bytes(&p.research_notes, HARD_PROMPT_CAP / 4));
    }
    prompt.push_str(
        "\n\nDecide EVERY assumption above yourself using best judgment and the research notes. \
REVISE the document(s) so each decision is baked in, add a 'Delegated decisions (auto)' section \
where every decision appears with a one-line rationale, and re-emit the COMPLETE revised \
document(s) — each in its OWN fenced block. Output NOTHING outside those fences — no JSON, no \
preamble, no prose.\n",
    );
    // Fence hardening (item 1): the last lines are the exact per-doc wrappers.
    prompt.push_str(&fence_reminder(tags));
    (system, truncate_bytes(&prompt, HARD_PROMPT_CAP))
}

/// Parse the safeguard gate's `well-known: yes|no` line (design-speedup item 4, `research_mode ==
/// "auto"`) from an enumerate-pass reply. `Some(true)` when the model reported the stack is
/// mainstream/well-known (skip research), `Some(false)` when not, `None` when the line is
/// absent/unparseable (the kernel then defaults to running research). Tolerant: case-insensitive,
/// matches the first `well-known:`/`well known:` line and reads a leading yes/true vs no/false.
pub fn parse_well_known(text: &str) -> Option<bool> {
    for line in text.lines() {
        let l = line.trim().to_ascii_lowercase();
        let rest = l
            .strip_prefix("well-known:")
            .or_else(|| l.strip_prefix("well known:"))
            .or_else(|| l.strip_prefix("- well-known:"))
            .or_else(|| l.strip_prefix("well-known"))
            .map(|s| s.trim_start_matches([':', ' ', '-']));
        if let Some(v) = rest {
            let v = v.trim();
            if v.starts_with("yes") || v.starts_with("true") {
                return Some(true);
            }
            if v.starts_with("no") || v.starts_with("false") {
                return Some(false);
            }
        }
    }
    None
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
    /// Optional ordered sprint grouping (feature: sprints), project track only. Each sprint carries
    /// a one-line goal and the task slugs it delivers. ABSENT / empty is fully back-compatible — the
    /// validator then falls back to a single implicit sprint of every task. `#[serde(default)]`.
    #[serde(default)]
    sprints: Vec<RawSprint>,
}

#[derive(Debug, Deserialize)]
struct RawSprint {
    #[serde(default)]
    goal: String,
    #[serde(default)]
    tasks: Vec<String>,
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
    /// Ordered sprint grouping (feature: sprints): each entry is a `(goal, task-slugs)` pair. ALWAYS
    /// non-empty and a partition of every task slug — [`parse_breakdown`] normalizes a missing /
    /// partial / garbage `sprints` array into a single implicit sprint of all tasks, and appends any
    /// task the model forgot to the last sprint, so `apply_breakdown` never has to reason about it.
    sprints: Vec<VSprint>,
}

/// One validated sprint (feature: sprints): a goal + the raw task slugs it delivers, in order.
#[derive(Clone, Debug, PartialEq, Eq)]
struct VSprint {
    goal: String,
    tasks: Vec<String>,
}

impl Breakdown {
    /// Total task count across every epic/story (feature: sdlc-triage): the kernel uses this to
    /// escalate an ENHANCEMENT whose breakdown returned more than 3 tasks up to the full project
    /// track (wider scope than a change).
    pub fn task_count(&self) -> usize {
        self.epics
            .iter()
            .flat_map(|e| e.stories.iter())
            .map(|s| s.tasks.len())
            .sum()
    }
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

    // Sprint grouping (feature: sprints) is LENIENT — it never rejects a breakdown. The ordered
    // task slugs (board order) are the partition domain; `group_sprints` assigns them to the
    // model's sprints when present and valid, and falls back to a single all-tasks sprint otherwise.
    let ordered: Vec<String> = epics
        .iter()
        .flat_map(|e| e.stories.iter())
        .flat_map(|s| s.tasks.iter())
        .map(|t| t.slug.clone())
        .collect();
    let sprints = group_sprints(&ordered, &parsed.sprints);

    Ok(Breakdown { epics, sprints })
}

/// The goal stamped on a single implicit sprint (feature: sprints) when the breakdown carried no
/// usable sprint grouping — the whole plan is one increment.
pub const DEFAULT_SPRINT_GOAL: &str = "Deliver the planned work";

/// Group the ordered task slugs into sprints (feature: sprints), LENIENTLY — this never fails.
/// `ordered` is every task slug in board order; `raw` is the model's (possibly absent / partial /
/// garbage) `sprints` array. Rules, in order:
///   1. For each raw sprint, keep only slugs that EXIST and were not already claimed by an earlier
///      sprint (dedupe + drop unknowns); a sprint that ends up empty is dropped.
///   2. Any task not claimed by a surviving sprint is appended to the LAST surviving sprint, so the
///      result is always a partition of every task (nothing is ever orphaned).
///   3. If no sprint survives (no `sprints`, all empty/garbage), fall back to ONE sprint of every
///      task with [`DEFAULT_SPRINT_GOAL`].
///
/// The output is ALWAYS non-empty and covers every slug exactly once.
fn group_sprints(ordered: &[String], raw: &[RawSprint]) -> Vec<VSprint> {
    let known: std::collections::HashSet<&str> = ordered.iter().map(String::as_str).collect();
    let mut claimed: std::collections::HashSet<String> = std::collections::HashSet::new();
    let mut sprints: Vec<VSprint> = Vec::new();

    for rs in raw {
        let mut tasks: Vec<String> = Vec::new();
        for slug in &rs.tasks {
            if known.contains(slug.as_str()) && claimed.insert(slug.clone()) {
                tasks.push(slug.clone());
            }
        }
        if tasks.is_empty() {
            continue; // an all-unknown / already-claimed / empty sprint contributes nothing
        }
        let goal = if rs.goal.trim().is_empty() {
            DEFAULT_SPRINT_GOAL.to_string()
        } else {
            rs.goal.trim().to_string()
        };
        sprints.push(VSprint { goal, tasks });
    }

    // Any task the model forgot to place goes into the last surviving sprint (in board order).
    let leftover: Vec<String> = ordered.iter().filter(|s| !claimed.contains(*s)).cloned().collect();
    if sprints.is_empty() {
        // No usable grouping at all -> one implicit sprint of every task (the back-compat default).
        return vec![VSprint {
            goal: DEFAULT_SPRINT_GOAL.to_string(),
            tasks: ordered.to_vec(),
        }];
    }
    if !leftover.is_empty() {
        if let Some(last) = sprints.last_mut() {
            last.tasks.extend(leftover);
        }
    }
    sprints
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
        diff_stat: None,
        awaiting_merge: false,
        dispatch_after_ms: 0,
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
                    diff_stat: None,
                    awaiting_merge: false,
                    dispatch_after_ms: 0,
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

    // Sprints (feature: sprints): resolve each sprint's raw slugs to full task ids via the same map,
    // and mark the FIRST sprint Active (dispatch scopes to it the moment the project is authorized),
    // the rest Pending. `group_sprints` already guaranteed a non-empty partition of every task.
    p.sprints = build_sprints(&b.sprints, &task_id);

    if let Ok(phase) = step_project(&p.phase, ProjectTransition::AcceptBreakdown) {
        p.phase = phase;
    }
}

/// Build the domain [`Sprint`] list from the validated `VSprint`s and the slug -> full-`TaskId` map
/// (feature: sprints). The FIRST sprint is `Active` (the office grinds it first), every later one
/// `Pending`. Unknown slugs are filtered defensively (they cannot occur — the map is built from the
/// same tasks — but never trust the model's slugs blindly). A resulting empty sprint is dropped;
/// if that somehow leaves nothing, the caller keeps the pre-existing (empty) sprint list, which the
/// kernel treats as the legacy no-sprint flow.
fn build_sprints(
    vsprints: &[VSprint],
    task_id: &std::collections::HashMap<String, TaskId>,
) -> Vec<Sprint> {
    let mut out: Vec<Sprint> = Vec::new();
    for vs in vsprints {
        let tasks: Vec<TaskId> = vs.tasks.iter().filter_map(|s| task_id.get(s).cloned()).collect();
        if tasks.is_empty() {
            continue;
        }
        out.push(Sprint {
            goal: vs.goal.clone(),
            tasks,
            status: SprintStatus::Pending,
            transcript: Vec::new(),
        });
    }
    if let Some(first) = out.first_mut() {
        first.status = SprintStatus::Active;
    }
    out
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
