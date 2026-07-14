//! Inbox tests (BUILD_WAVES.md W8): tempdir-rooted, no `Host` dependency. Covers the
//! six ops, malformed/unknown-op rejection, processed/rejected file moves, and the
//! per-tick flood cap.

use super::*;
use crate::handlers::Command;
use std::fs;
use tempfile::TempDir;

fn write_inbox_file(dir: &Path, name: &str, body: &str) {
    fs::write(dir.join(name), body).expect("write inbox file");
}

// ---------------------------------------------------------------------------
// Happy path per op
// ---------------------------------------------------------------------------

#[test]
fn brief_parses_and_moves_to_processed() {
    let tmp = TempDir::new().unwrap();
    let inbox = tmp.path();
    write_inbox_file(
        inbox,
        "1000-brief.json",
        r#"{"op":"brief","project":"shop","message":"add a cart"}"#,
    );

    let outcomes = poll(inbox, MAX_FILES_PER_TICK);
    assert_eq!(outcomes.len(), 1);
    match &outcomes[0] {
        InboxOutcome::Accepted { file, command, ack } => {
            assert_eq!(file, "1000-brief.json");
            assert_eq!(
                *command,
                Command::Brief {
                    project: Some("shop".to_string()),
                    message: "add a cart".to_string(),
                }
            );
            assert_eq!(ack, "office is thinking; answer will arrive via chat");
        }
        other => panic!("expected Accepted, got {other:?}"),
    }

    assert!(!inbox.join("1000-brief.json").exists());
    assert!(inbox.join("processed").join("1000-brief.json").exists());
}

#[test]
fn brief_without_project_defaults_to_none() {
    let tmp = TempDir::new().unwrap();
    let inbox = tmp.path();
    write_inbox_file(inbox, "1-brief.json", r#"{"op":"brief","message":"hello"}"#);

    let outcomes = poll(inbox, MAX_FILES_PER_TICK);
    match &outcomes[0] {
        InboxOutcome::Accepted { command, .. } => {
            assert_eq!(
                *command,
                Command::Brief {
                    project: None,
                    message: "hello".to_string(),
                }
            );
        }
        other => panic!("expected Accepted, got {other:?}"),
    }
}

#[test]
fn status_parses() {
    let tmp = TempDir::new().unwrap();
    let inbox = tmp.path();
    write_inbox_file(inbox, "2-status.json", r#"{"op":"status","project":"shop"}"#);

    let outcomes = poll(inbox, MAX_FILES_PER_TICK);
    match &outcomes[0] {
        InboxOutcome::Accepted { command, ack, .. } => {
            assert_eq!(
                *command,
                Command::Status { project: Some("shop".to_string()) }
            );
            assert_eq!(ack, "queued");
        }
        other => panic!("expected Accepted, got {other:?}"),
    }
}

#[test]
fn authorize_parses() {
    let tmp = TempDir::new().unwrap();
    let inbox = tmp.path();
    write_inbox_file(
        inbox,
        "3-authorize.json",
        r#"{"op":"authorize","project":"shop","delivery_path":"/tmp/out"}"#,
    );

    let outcomes = poll(inbox, MAX_FILES_PER_TICK);
    match &outcomes[0] {
        InboxOutcome::Accepted { command, .. } => {
            assert_eq!(
                *command,
                Command::Authorize {
                    project: "shop".to_string(),
                    delivery_path: "/tmp/out".to_string(),
                }
            );
        }
        other => panic!("expected Accepted, got {other:?}"),
    }
}

#[test]
fn interrupt_defaults_hard_unless_mode_is_soft() {
    let tmp = TempDir::new().unwrap();
    let inbox = tmp.path();
    write_inbox_file(inbox, "4-hard.json", r#"{"op":"interrupt","project":"shop"}"#);
    write_inbox_file(
        inbox,
        "5-soft.json",
        r#"{"op":"interrupt","project":"shop","mode":"soft"}"#,
    );

    let mut outcomes = poll(inbox, MAX_FILES_PER_TICK);
    outcomes.sort_by_key(|o| match o {
        InboxOutcome::Accepted { file, .. } => file.clone(),
        InboxOutcome::Rejected { file, .. } => file.clone(),
    });

    match &outcomes[0] {
        InboxOutcome::Accepted { command, .. } => {
            assert_eq!(
                *command,
                Command::Interrupt { project: "shop".to_string(), hard: true }
            );
        }
        other => panic!("expected Accepted, got {other:?}"),
    }
    match &outcomes[1] {
        InboxOutcome::Accepted { command, .. } => {
            assert_eq!(
                *command,
                Command::Interrupt { project: "shop".to_string(), hard: false }
            );
        }
        other => panic!("expected Accepted, got {other:?}"),
    }
}

#[test]
fn resume_parses() {
    let tmp = TempDir::new().unwrap();
    let inbox = tmp.path();
    write_inbox_file(inbox, "6-resume.json", r#"{"op":"resume","project":"shop"}"#);

    let outcomes = poll(inbox, MAX_FILES_PER_TICK);
    match &outcomes[0] {
        InboxOutcome::Accepted { command, .. } => {
            assert_eq!(*command, Command::Resume { project: "shop".to_string() });
        }
        other => panic!("expected Accepted, got {other:?}"),
    }
}

#[test]
fn comment_parses() {
    let tmp = TempDir::new().unwrap();
    let inbox = tmp.path();
    write_inbox_file(
        inbox,
        "7-comment.json",
        r#"{"op":"comment","task":"t1","text":"looks good"}"#,
    );

    let outcomes = poll(inbox, MAX_FILES_PER_TICK);
    match &outcomes[0] {
        InboxOutcome::Accepted { command, .. } => {
            assert_eq!(
                *command,
                Command::Comment { task: "t1".to_string(), text: "looks good".to_string() }
            );
        }
        other => panic!("expected Accepted, got {other:?}"),
    }
}

// ---------------------------------------------------------------------------
// Tolerance: malformed / unknown op never panics, always rejected
// ---------------------------------------------------------------------------

#[test]
fn malformed_json_is_rejected_not_panicked() {
    let tmp = TempDir::new().unwrap();
    let inbox = tmp.path();
    write_inbox_file(inbox, "8-bad.json", "{ this is not json");

    let outcomes = poll(inbox, MAX_FILES_PER_TICK);
    assert_eq!(outcomes.len(), 1);
    match &outcomes[0] {
        InboxOutcome::Rejected { file, reason } => {
            assert_eq!(file, "8-bad.json");
            assert!(reason.contains("invalid JSON"), "reason: {reason}");
        }
        other => panic!("expected Rejected, got {other:?}"),
    }
    assert!(!inbox.join("8-bad.json").exists());
    assert!(inbox.join("rejected").join("8-bad.json").exists());
    assert!(inbox.join("rejected").join("8-bad.json.error").exists());
}

#[test]
fn unknown_op_is_rejected() {
    let tmp = TempDir::new().unwrap();
    let inbox = tmp.path();
    write_inbox_file(inbox, "9-unknown.json", r#"{"op":"launch_missiles"}"#);

    let outcomes = poll(inbox, MAX_FILES_PER_TICK);
    match &outcomes[0] {
        InboxOutcome::Rejected { reason, .. } => {
            assert!(reason.contains("unknown op"), "reason: {reason}");
        }
        other => panic!("expected Rejected, got {other:?}"),
    }
}

#[test]
fn missing_op_is_rejected() {
    let tmp = TempDir::new().unwrap();
    let inbox = tmp.path();
    write_inbox_file(inbox, "10-noop.json", r#"{"project":"shop"}"#);

    let outcomes = poll(inbox, MAX_FILES_PER_TICK);
    match &outcomes[0] {
        InboxOutcome::Rejected { reason, .. } => {
            assert!(reason.contains("op"), "reason: {reason}");
        }
        other => panic!("expected Rejected, got {other:?}"),
    }
}

#[test]
fn missing_required_field_is_rejected() {
    let tmp = TempDir::new().unwrap();
    let inbox = tmp.path();
    write_inbox_file(inbox, "11-comment.json", r#"{"op":"comment","task":"t1"}"#);

    let outcomes = poll(inbox, MAX_FILES_PER_TICK);
    match &outcomes[0] {
        InboxOutcome::Rejected { reason, .. } => {
            assert!(reason.contains("text"), "reason: {reason}");
        }
        other => panic!("expected Rejected, got {other:?}"),
    }
}

#[test]
fn non_object_json_is_rejected() {
    let tmp = TempDir::new().unwrap();
    let inbox = tmp.path();
    write_inbox_file(inbox, "12-array.json", r#"["op","brief"]"#);

    let outcomes = poll(inbox, MAX_FILES_PER_TICK);
    match &outcomes[0] {
        InboxOutcome::Rejected { reason, .. } => {
            assert!(reason.contains("JSON object"), "reason: {reason}");
        }
        other => panic!("expected Rejected, got {other:?}"),
    }
}

// ---------------------------------------------------------------------------
// Flood cap
// ---------------------------------------------------------------------------

#[test]
fn flood_cap_bounds_files_consumed_per_tick() {
    let tmp = TempDir::new().unwrap();
    let inbox = tmp.path();
    for i in 0..10 {
        write_inbox_file(
            inbox,
            &format!("{i:04}-resume.json", i = i),
            r#"{"op":"resume","project":"shop"}"#,
        );
    }

    let outcomes = poll(inbox, 3);
    assert_eq!(outcomes.len(), 3);

    // Only the 3 lexicographically-first files (i.e. earliest millis) were consumed;
    // the rest remain in the inbox untouched for the next tick.
    let remaining: usize = fs::read_dir(inbox)
        .unwrap()
        .filter_map(|e| e.ok())
        .filter(|e| e.path().extension().and_then(|x| x.to_str()) == Some("json"))
        .count();
    assert_eq!(remaining, 7);

    let processed: usize = fs::read_dir(inbox.join("processed")).unwrap().count();
    assert_eq!(processed, 3);
}

#[test]
fn no_inbox_dir_yields_no_outcomes_without_panicking() {
    let tmp = TempDir::new().unwrap();
    let missing = tmp.path().join("does-not-exist");
    let outcomes = poll(&missing, MAX_FILES_PER_TICK);
    assert!(outcomes.is_empty());
}

#[test]
fn non_json_files_are_ignored() {
    let tmp = TempDir::new().unwrap();
    let inbox = tmp.path();
    write_inbox_file(inbox, "notes.txt", "not for us");
    write_inbox_file(inbox, "1-resume.json", r#"{"op":"resume","project":"shop"}"#);

    let outcomes = poll(inbox, MAX_FILES_PER_TICK);
    assert_eq!(outcomes.len(), 1);
    assert!(inbox.join("notes.txt").exists());
}
