//! Tolerant `OFFICE-REPORT` / `OFFICE-REVIEW` trailer scanner (ARCHITECTURE.md 8.3).
//!
//! Model output is sloppy: prose before/after the block, markdown fences, case
//! drift on the marker and keys, duplicate blocks. The scanner is deliberately
//! forgiving: find the LAST marker line, collect `key: value` lines (continuation
//! lines fold into the previously-seen key) until a blank line or EOF, ignore
//! unknown keys, and never panic on garbage input — a missing block just yields
//! `Unparseable` (ARCHITECTURE.md 5.3: the kernel treats that as complete-with-
//! warning rather than stalling the line).

use crate::domain::CommentId;
use std::collections::HashMap;

/// Parsed `status:` value from an `OFFICE-REPORT` trailer.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ReportStatus {
    Complete,
    Blocked,
    /// No `OFFICE-REPORT` block was found at all, or its `status:` value was
    /// missing/unrecognized.
    Unparseable,
}

impl Default for ReportStatus {
    fn default() -> Self {
        ReportStatus::Unparseable
    }
}

/// The parsed worker report trailer.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ReportTrailer {
    pub status: ReportStatus,
    pub summary: Option<String>,
    /// Newline-separated absolute paths the worker reported (best-effort; not
    /// validated against the filesystem here — that is the driver's job).
    pub delivered: Vec<String>,
    pub ack_comments: Vec<CommentId>,
    pub blocked_reason: Option<String>,
}

/// Parsed `verdict:` value from an `OFFICE-REVIEW` trailer.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Verdict {
    Pass,
    Fail,
    /// No `OFFICE-REVIEW` block was found, or `verdict:` was missing/unrecognized.
    Unparseable,
}

impl Default for Verdict {
    fn default() -> Self {
        Verdict::Unparseable
    }
}

/// The parsed reviewer verdict trailer.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ReviewTrailer {
    pub verdict: Verdict,
    pub reasons: Option<String>,
    /// Optional per-task clean-build hygiene grade (0-100) the reviewer may emit on a PASS
    /// (item 3, rolling score). `None` when the reviewer omitted the `hygiene:` line — the
    /// kernel treats an absent grade as 100 so older reviewers stay fully compatible. Clamped
    /// to `..=100` (the first digit run on the line, so `hygiene: 92/100` also reads 92).
    pub hygiene: Option<u32>,
}

/// Strip a run of backtick characters (markdown fence) from `line`, trimmed. A
/// fence-only line (e.g. ` ``` ` or "```text") normalizes to the empty string.
fn strip_fence(line: &str) -> &str {
    line.trim().trim_matches('`')
}

fn is_fence_only(line: &str) -> bool {
    let t = line.trim();
    !t.is_empty() && t.chars().all(|c| c == '`')
}

/// Find the LAST line matching `marker` (case-insensitive, fence-tolerant) and
/// scan forward collecting `key: value` lines (known keys only trigger a new
/// field; everything else folds into whichever field is currently open) until a
/// blank line or EOF. Returns `None` if the marker never appears.
fn scan_block(text: &str, marker: &str, known_keys: &[&str]) -> Option<HashMap<String, Vec<String>>> {
    let lines: Vec<&str> = text.lines().collect();
    let marker_lower = marker.to_ascii_lowercase();

    let marker_idx = lines
        .iter()
        .enumerate()
        .filter(|(_, l)| strip_fence(l).eq_ignore_ascii_case(&marker_lower))
        .map(|(i, _)| i)
        .last()?;

    let mut map: HashMap<String, Vec<String>> = HashMap::new();
    let mut current_key: Option<String> = None;

    for line in &lines[marker_idx + 1..] {
        if line.trim().is_empty() {
            break;
        }
        if is_fence_only(line) {
            continue;
        }

        if let Some((raw_key, raw_val)) = line.split_once(':') {
            let key_lower = raw_key.trim().to_ascii_lowercase();
            if known_keys.contains(&key_lower.as_str()) {
                let val = raw_val.trim().to_string();
                map.entry(key_lower.clone()).or_default().push(val);
                current_key = Some(key_lower);
                continue;
            }
        }

        // Continuation line for whichever known key is currently open; lines
        // before any recognized key (stray prose) are ignored.
        if let Some(key) = &current_key {
            let val = line.trim().to_string();
            if !val.is_empty() {
                map.entry(key.clone()).or_default().push(val);
            }
        }
    }

    Some(map)
}

fn joined(map: &HashMap<String, Vec<String>>, key: &str) -> Option<String> {
    map.get(key).map(|lines| lines.join("\n")).filter(|s| !s.trim().is_empty())
}

/// Parse comma/whitespace-separated `c<n>` tokens (across every line folded into
/// the `ack-comments` field) into `CommentId`s. Malformed tokens are ignored.
fn parse_ack_comments(map: &HashMap<String, Vec<String>>) -> Vec<CommentId> {
    let raw = match map.get("ack-comments") {
        Some(v) => v.join(","),
        None => return Vec::new(),
    };
    raw.split(|c: char| c == ',' || c.is_whitespace())
        .filter(|tok| !tok.is_empty())
        .filter_map(|tok| {
            let digits = tok.strip_prefix(|c: char| c == 'c' || c == 'C').unwrap_or(tok);
            digits.parse::<u64>().ok().map(CommentId)
        })
        .collect()
}

const REPORT_KEYS: &[&str] = &["status", "summary", "delivered", "ack-comments", "blocked-reason"];
const REVIEW_KEYS: &[&str] = &["verdict", "reasons", "hygiene"];
const RESEARCH_KEYS: &[&str] = &["findings"];
const AUDIT_KEYS: &[&str] = &["grade", "failures"];
const ASSUME_KEYS: &[&str] = &["verdict"];
const TRIAGE_KEYS: &[&str] = &["track", "rationale", "existing"];
const SPRINT_REVIEW_KEYS: &[&str] = &["summary", "adjustments"];

/// A parsed `OFFICE-AUDIT` block (ARCHITECTURE.md 6.2c). `grade` is `None` when no numeric
/// grade could be read (an inconclusive audit); the kernel fails OPEN on that (completes with a
/// notice) rather than punishing the delivery for the auditor's formatting slip.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct AuditReport {
    pub grade: Option<u32>,
    pub failures: Vec<String>,
}

/// The verdict of an `ASSUME-CHECK` block (ARCHITECTURE.md 6.2c safeguard gate).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AssumeVerdict {
    Clean,
    Assumptions,
}

/// A parsed `ASSUME-CHECK` block. `items` are the ungrounded assumptions the safeguard listed
/// (empty on a clean verdict).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AssumeCheck {
    pub verdict: AssumeVerdict,
    pub items: Vec<String>,
}

/// Parse the LAST `OFFICE-RESEARCH` block's `findings` value out of `text` (ARCHITECTURE.md
/// 6.2b), tolerant exactly like [`parse_report`]/[`parse_review`]: fence-tolerant marker
/// match, case drift ignored, continuation lines folded into `findings`. `None` when no block
/// is present — the caller then falls back to the whole reply text (a researcher that skipped
/// the block still yields usable notes).
pub fn parse_research(text: &str) -> Option<String> {
    let map = scan_block(text, "OFFICE-RESEARCH", RESEARCH_KEYS)?;
    joined(&map, "findings")
}

/// Strip a leading markdown/bullet marker (`- `, `* `, `1. `) and surrounding whitespace from a
/// list line, so folded `failures:`/assumption lines read as plain items.
fn strip_bullet(line: &str) -> String {
    let t = line.trim();
    let t = t
        .strip_prefix("- ")
        .or_else(|| t.strip_prefix("* "))
        .or_else(|| t.strip_prefix("• "))
        .unwrap_or(t);
    t.trim().to_string()
}

/// Parse the LAST `OFFICE-AUDIT` block out of `text` (ARCHITECTURE.md 6.2c), tolerant like the
/// other trailer scanners. `grade:` is read as a clamped 0..=100 integer (the first run of
/// digits on the line, so `grade: 87/100` or `grade: 87 (pass)` both read 87); the `failures:`
/// value and its folded continuation lines become the failure items (bullets stripped).
pub fn parse_audit(text: &str) -> AuditReport {
    let map = match scan_block(text, "OFFICE-AUDIT", AUDIT_KEYS) {
        Some(m) => m,
        None => return AuditReport::default(),
    };

    let grade = map
        .get("grade")
        .and_then(|v| v.first())
        .and_then(|s| parse_first_u32(s))
        .map(|n| n.min(100));

    let failures = map
        .get("failures")
        .map(|lines| {
            lines
                .iter()
                .map(|l| strip_bullet(l))
                .filter(|l| !l.is_empty())
                .collect()
        })
        .unwrap_or_default();

    AuditReport { grade, failures }
}

/// The first contiguous run of ASCII digits in `s`, parsed as `u32` (tolerates `87/100`,
/// `grade 87`, surrounding prose).
fn parse_first_u32(s: &str) -> Option<u32> {
    let digits: String = s
        .chars()
        .skip_while(|c| !c.is_ascii_digit())
        .take_while(|c| c.is_ascii_digit())
        .collect();
    digits.parse::<u32>().ok()
}

/// Parse the LAST `ASSUME-CHECK` block out of `text` (ARCHITECTURE.md 6.2c). The block is
/// `verdict: clean|assumptions` followed by bare `- <item>` lines; since the scanner folds
/// keyless continuation lines into the open `verdict` key, the FIRST folded line is the verdict
/// word and the rest are the assumption items. `None` when no block is present — the kernel
/// fails OPEN on that (proceeds) rather than wedging the pipeline.
pub fn parse_assume_check(text: &str) -> Option<AssumeCheck> {
    let map = scan_block(text, "ASSUME-CHECK", ASSUME_KEYS)?;
    let lines = map.get("verdict")?;
    let verdict_word = lines.first()?.trim().to_ascii_lowercase();
    let verdict = if verdict_word.starts_with("clean") {
        AssumeVerdict::Clean
    } else {
        AssumeVerdict::Assumptions
    };
    let items: Vec<String> = lines
        .iter()
        .skip(1)
        .map(|l| strip_bullet(l))
        .filter(|l| !l.is_empty())
        .collect();
    Some(AssumeCheck { verdict, items })
}

/// One flagged assumption after criticality classification (autonomous-safeguard pivot). `critical`
/// items freeze the pipeline for the human; the rest are auto-resolved. `text` is the item with its
/// `[critical]`/`[auto]` tag stripped, ready for display / prompts.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ClassifiedAssumption {
    pub critical: bool,
    pub text: String,
}

/// Classify one `ASSUME-CHECK` item by its leading criticality tag (autonomous-safeguard pivot).
/// Tolerant: a leading `[critical]` (case-insensitive, any surrounding whitespace) marks it
/// critical; a leading `[auto]` marks it auto; ANYTHING ELSE — including an untagged item — is
/// treated as `[auto]` (the safe default: the office decides it, no human freeze). The returned
/// `text` has the tag and any immediately-following `:`/`-`/whitespace stripped. An empty result
/// text (e.g. a bare `[critical]` with no item) is left empty for the caller to drop.
pub fn classify_assumption(item: &str) -> ClassifiedAssumption {
    let t = item.trim();
    if let Some(rest) = strip_leading_tag(t, "[critical]") {
        return ClassifiedAssumption { critical: true, text: rest };
    }
    if let Some(rest) = strip_leading_tag(t, "[auto]") {
        return ClassifiedAssumption { critical: false, text: rest };
    }
    ClassifiedAssumption { critical: false, text: t.to_string() }
}

/// Strip a leading bracket tag (`"[critical]"` / `"[auto]"`) case-insensitively, then any
/// immediately-following separator (`:`/`-`/whitespace). `None` when `s` does not start with `tag`.
/// Uses `get(..len)` so a multibyte boundary right at the tag length can never panic.
fn strip_leading_tag(s: &str, tag: &str) -> Option<String> {
    let head = s.get(..tag.len())?;
    if head.eq_ignore_ascii_case(tag) {
        let rest = s[tag.len()..].trim_start_matches(|c: char| c == ':' || c == '-' || c.is_whitespace());
        Some(rest.to_string())
    } else {
        None
    }
}

/// The SDLC intake track a brief is classified into (feature: sdlc-triage). `Project` = the full
/// PRD/TRD/CRD ceremony (the safe default); `Enhancement` = one change-brief + a small breakdown;
/// `Patch` = no documents, one task straight to Ready.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TriageTrack {
    Project,
    Enhancement,
    Patch,
}

impl TriageTrack {
    /// The persisted string form stored on `Project.track` and rendered on the wire digests.
    pub fn as_str(self) -> &'static str {
        match self {
            TriageTrack::Project => "project",
            TriageTrack::Enhancement => "enhancement",
            TriageTrack::Patch => "patch",
        }
    }
}

/// A parsed `SDLC-TRIAGE` block (feature: sdlc-triage). `track` is the classified track;
/// `rationale` a one-line justification; `existing` whether the brief targets an EXISTING delivery
/// (only meaningful for enhancement/patch). Parsing NEVER fails — a missing/garbled block yields the
/// [`TriageVerdict::project_default`] (full ceremony is the safe fallback).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TriageVerdict {
    pub track: TriageTrack,
    pub rationale: String,
    pub existing: bool,
}

impl TriageVerdict {
    /// The defensive default: the full-ceremony `project` track, used when the classifier block is
    /// absent or unparseable, or the invoke errored.
    pub fn project_default() -> Self {
        TriageVerdict {
            track: TriageTrack::Project,
            rationale: String::new(),
            existing: false,
        }
    }
}

/// Classify a raw `track:` value word into a [`TriageTrack`] (feature: sdlc-triage). Tolerant and
/// conservative: a leading `enhance`/`patch` (case-insensitive) picks that track; ANYTHING ELSE —
/// including `project`, an empty value, or garbage — is the safe `Project` default.
fn classify_track(word: &str) -> TriageTrack {
    let w = word.trim().to_ascii_lowercase();
    if w.starts_with("enhance") {
        TriageTrack::Enhancement
    } else if w.starts_with("patch") {
        TriageTrack::Patch
    } else {
        TriageTrack::Project
    }
}

/// Parse the LAST `SDLC-TRIAGE` block out of `text` (feature: sdlc-triage), tolerant exactly like
/// the other trailer scanners: fence-tolerant marker match, case drift ignored, continuation lines
/// folded. `track:` is classified by [`classify_track`] (unknown -> `Project`); `existing:` reads a
/// leading yes/true. A MISSING block (or any unrecognized track) yields
/// [`TriageVerdict::project_default`] — the full ceremony is the safe fallback, never a hard error.
pub fn parse_triage(text: &str) -> TriageVerdict {
    let map = match scan_block(text, "SDLC-TRIAGE", TRIAGE_KEYS) {
        Some(m) => m,
        None => return TriageVerdict::project_default(),
    };
    let track = map
        .get("track")
        .and_then(|v| v.first())
        .map(|s| classify_track(s))
        .unwrap_or(TriageTrack::Project);
    let rationale = joined(&map, "rationale").unwrap_or_default();
    let existing = map
        .get("existing")
        .and_then(|v| v.first())
        .map(|s| {
            let v = s.trim().to_ascii_lowercase();
            v.starts_with("yes") || v.starts_with("true")
        })
        .unwrap_or(false);
    TriageVerdict {
        track,
        rationale,
        existing,
    }
}

/// What a sprint-review PM adjustment does to the NEXT sprint's task list (feature: sprints).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SprintAdjustmentKind {
    /// Remove a not-yet-started task from the next sprint (and the board).
    Drop,
    /// Add a fresh task to the next sprint.
    Add,
    /// Replace a not-yet-started task's description.
    Modify,
}

/// One parsed adjustment from a `SPRINT-REVIEW` block (feature: sprints). `target` is the exact task
/// id/slug for `Drop`/`Modify`, or the new task title for `Add`; `text` is the description for
/// `Add`/`Modify` (empty for `Drop`). The kernel applies these DEFENSIVELY — a target it can't match
/// (or a task already started/done) is simply ignored.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SprintAdjustment {
    pub kind: SprintAdjustmentKind,
    pub target: String,
    pub text: String,
}

/// The parsed `SPRINT-REVIEW` synthesis (feature: sprints). `summary` is the PM's closing line(s);
/// `adjustments` are the (possibly empty) changes to the next sprint. Parsing NEVER fails — a
/// missing/garbled block yields the default (empty summary, no adjustments), so the ceremony always
/// falls back to "carry-overs only".
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SprintReviewPlan {
    pub summary: String,
    pub adjustments: Vec<SprintAdjustment>,
}

/// Parse the LAST `SPRINT-REVIEW` block out of `text` (feature: sprints), tolerant exactly like the
/// other trailer scanners: fence-tolerant marker, case drift ignored, continuation lines folded. The
/// `summary:` value (+ folded lines) is the PM synthesis; each `- drop|add|modify ...` line under
/// `adjustments:` becomes a [`SprintAdjustment`]. A missing block or an unrecognized verb yields no
/// changes — the kernel then carries over the non-done tasks and makes no edits.
pub fn parse_sprint_review(text: &str) -> SprintReviewPlan {
    let map = match scan_block(text, "SPRINT-REVIEW", SPRINT_REVIEW_KEYS) {
        Some(m) => m,
        None => return SprintReviewPlan::default(),
    };
    let summary = joined(&map, "summary").unwrap_or_default();
    let adjustments = map
        .get("adjustments")
        .map(|lines| parse_adjustments(lines))
        .unwrap_or_default();
    SprintReviewPlan { summary, adjustments }
}

/// Parse the folded `adjustments:` lines into [`SprintAdjustment`]s (feature: sprints). Each line is
/// bullet-stripped, then classified by its leading verb (`drop`/`add`/`modify`, case-insensitive);
/// `add`/`modify` split their remainder on the first `|` into `(target, text)`. An empty target, or
/// any line whose first token is not a known verb, is dropped.
fn parse_adjustments(lines: &[String]) -> Vec<SprintAdjustment> {
    let mut out = Vec::new();
    for raw in lines {
        let line = strip_bullet(raw);
        if let Some(rest) = strip_verb(&line, "drop") {
            let target = rest.trim().to_string();
            if !target.is_empty() {
                out.push(SprintAdjustment { kind: SprintAdjustmentKind::Drop, target, text: String::new() });
            }
        } else if let Some(rest) = strip_verb(&line, "add") {
            let (target, text) = split_pipe(&rest);
            if !target.is_empty() {
                out.push(SprintAdjustment { kind: SprintAdjustmentKind::Add, target, text });
            }
        } else if let Some(rest) = strip_verb(&line, "modify") {
            let (target, text) = split_pipe(&rest);
            if !target.is_empty() {
                out.push(SprintAdjustment { kind: SprintAdjustmentKind::Modify, target, text });
            }
        }
        // any other leading word is ignored (defensive)
    }
    out
}

/// Strip a leading verb keyword (case-insensitive) plus its trailing `:`/whitespace, returning the
/// remainder. `None` when the FIRST whitespace/':'-delimited token is not exactly `verb`.
fn strip_verb(line: &str, verb: &str) -> Option<String> {
    let t = line.trim_start();
    let first_len = t
        .find(|c: char| c.is_whitespace() || c == ':')
        .unwrap_or(t.len());
    if t[..first_len].eq_ignore_ascii_case(verb) {
        Some(t[first_len..].trim_start_matches(|c: char| c == ':' || c.is_whitespace()).to_string())
    } else {
        None
    }
}

/// Split an `add`/`modify` remainder on the FIRST `|` into `(target, text)`, both trimmed. With no
/// `|` the whole remainder is the target and the text is empty.
fn split_pipe(s: &str) -> (String, String) {
    match s.split_once('|') {
        Some((a, b)) => (a.trim().to_string(), b.trim().to_string()),
        None => (s.trim().to_string(), String::new()),
    }
}

/// Parse the LAST `OFFICE-REPORT` trailer out of `text`.
pub fn parse_report(text: &str) -> ReportTrailer {
    let map = match scan_block(text, "OFFICE-REPORT", REPORT_KEYS) {
        Some(m) => m,
        None => return ReportTrailer::default(),
    };

    let status = match map.get("status").and_then(|v| v.first()) {
        Some(s) if s.trim().eq_ignore_ascii_case("complete") => ReportStatus::Complete,
        Some(s) if s.trim().eq_ignore_ascii_case("blocked") => ReportStatus::Blocked,
        _ => ReportStatus::Unparseable,
    };

    let delivered = map
        .get("delivered")
        .map(|lines| lines.iter().map(|l| l.trim().to_string()).filter(|l| !l.is_empty()).collect())
        .unwrap_or_default();

    ReportTrailer {
        status,
        summary: joined(&map, "summary"),
        delivered,
        ack_comments: parse_ack_comments(&map),
        blocked_reason: joined(&map, "blocked-reason"),
    }
}

/// Parse the LAST `OFFICE-REVIEW` trailer out of `text`.
pub fn parse_review(text: &str) -> ReviewTrailer {
    let map = match scan_block(text, "OFFICE-REVIEW", REVIEW_KEYS) {
        Some(m) => m,
        None => return ReviewTrailer::default(),
    };

    let verdict = match map.get("verdict").and_then(|v| v.first()) {
        Some(s) if s.trim().eq_ignore_ascii_case("pass") => Verdict::Pass,
        Some(s) if s.trim().eq_ignore_ascii_case("fail") => Verdict::Fail,
        _ => Verdict::Unparseable,
    };

    // Optional per-task hygiene grade (item 3): first digit run on the `hygiene:` line, clamped
    // 0..=100. Absent => None (the kernel treats that as 100 for the rolling average).
    let hygiene = map
        .get("hygiene")
        .and_then(|v| v.first())
        .and_then(|s| parse_first_u32(s))
        .map(|n| n.min(100));

    ReviewTrailer {
        verdict,
        reasons: joined(&map, "reasons"),
        hygiene,
    }
}
