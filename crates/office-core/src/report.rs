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

/// Parse the LAST `OFFICE-RESEARCH` block's `findings` value out of `text` (ARCHITECTURE.md
/// 6.2b), tolerant exactly like [`parse_report`]/[`parse_review`]: fence-tolerant marker
/// match, case drift ignored, continuation lines folded into `findings`. `None` when no block
/// is present — the caller then falls back to the whole reply text (a researcher that skipped
/// the block still yields usable notes).
pub fn parse_research(text: &str) -> Option<String> {
    let map = scan_block(text, "OFFICE-RESEARCH", RESEARCH_KEYS)?;
    joined(&map, "findings")
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
