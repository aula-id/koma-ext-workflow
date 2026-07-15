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
const REVIEW_KEYS: &[&str] = &["verdict", "reasons"];
const RESEARCH_KEYS: &[&str] = &["findings"];
const AUDIT_KEYS: &[&str] = &["grade", "failures"];
const ASSUME_KEYS: &[&str] = &["verdict"];

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

    ReviewTrailer {
        verdict,
        reasons: joined(&map, "reasons"),
    }
}
