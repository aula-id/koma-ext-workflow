//! Tests for inbox-dir resolution + monotonic file naming + the actual file write.

use super::*;
use std::path::Path;

// ---------------------------------------------------------------------------
// Inbox-dir resolution matrix (pure)
// ---------------------------------------------------------------------------

#[test]
fn explicit_workspace_arg_wins_over_everything() {
    let got = resolve_inbox_dir(
        Some("/ws/explicit"),
        Some("/ws/env"),
        Path::new("/cwd"),
        true, // cwd has a marker, but the explicit arg still wins
        Path::new("/home/.koma-workflow/inbox"),
    );
    assert_eq!(got, Path::new("/ws/explicit/koma-workflow/inbox"));
}

#[test]
fn env_workspace_used_when_no_arg() {
    let got = resolve_inbox_dir(
        None,
        Some("/ws/env"),
        Path::new("/cwd"),
        true,
        Path::new("/home/.koma-workflow/inbox"),
    );
    assert_eq!(got, Path::new("/ws/env/koma-workflow/inbox"));
}

#[test]
fn cwd_used_only_when_it_has_the_marker() {
    let got = resolve_inbox_dir(
        None,
        None,
        Path::new("/cwd"),
        true,
        Path::new("/home/.koma-workflow/inbox"),
    );
    assert_eq!(got, Path::new("/cwd/koma-workflow/inbox"));
}

#[test]
fn falls_back_to_global_when_cwd_has_no_marker() {
    let got = resolve_inbox_dir(
        None,
        None,
        Path::new("/cwd"),
        false, // no koma-workflow/ under cwd
        Path::new("/home/.koma-workflow/inbox"),
    );
    // Global inbox is used verbatim (NOT suffixed with koma-workflow/inbox).
    assert_eq!(got, Path::new("/home/.koma-workflow/inbox"));
}

#[test]
fn blank_workspace_and_env_are_ignored() {
    // Empty / whitespace-only workspace + env must not shadow the cwd/global fallbacks.
    let got = resolve_inbox_dir(
        Some("   "),
        Some(""),
        Path::new("/cwd"),
        false,
        Path::new("/home/.koma-workflow/inbox"),
    );
    assert_eq!(got, Path::new("/home/.koma-workflow/inbox"));
}

// ---------------------------------------------------------------------------
// Monotonic file naming
// ---------------------------------------------------------------------------

/// Parse the `(millis, counter)` out of a `<millis>-<counter>-mcp.json` filename.
fn parse_name(name: &str) -> (u64, u64) {
    let stem = name.strip_suffix("-mcp.json").expect("has -mcp.json suffix");
    let (millis, counter) = stem.split_once('-').expect("has millis-counter");
    (millis.parse().unwrap(), counter.parse().unwrap())
}

#[test]
fn filenames_are_unique_suffixed_and_strictly_increasing() {
    let names: Vec<String> = (0..8).map(|_| next_inbox_filename()).collect();

    // Every name carries the writer tag and is unique.
    for n in &names {
        assert!(n.ends_with("-mcp.json"), "name {n} must carry the -mcp tag");
    }
    let unique: std::collections::HashSet<&String> = names.iter().collect();
    assert_eq!(unique.len(), names.len(), "filenames must be unique");

    // (millis, counter) is strictly increasing across successive mints: millis never goes
    // backwards and the counter strictly increases, so the pair is monotonic.
    let parsed: Vec<(u64, u64)> = names.iter().map(|n| parse_name(n)).collect();
    for pair in parsed.windows(2) {
        assert!(pair[1] > pair[0], "names must be strictly increasing: {pair:?}");
    }
}

// ---------------------------------------------------------------------------
// The actual write
// ---------------------------------------------------------------------------

#[test]
fn write_inbox_file_creates_dir_and_writes_valid_json() {
    let tmp = tempfile::tempdir().unwrap();
    let inbox = tmp.path().join("nested").join("inbox"); // does not exist yet
    let body = office_core::inboxmsg::brief(Some("shop"), "add a cart");

    let path = write_inbox_file(&inbox, &body).expect("write");

    assert!(path.exists(), "the file was written");
    assert!(path.starts_with(&inbox), "written under the resolved inbox dir");
    let round: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&path).unwrap()).expect("valid json on disk");
    assert_eq!(round, body, "the on-disk body is exactly the builder output");
}
