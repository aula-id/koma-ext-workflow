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

#[test]
fn breakdown_parses() {
    let tmp = TempDir::new().unwrap();
    let inbox = tmp.path();
    write_inbox_file(inbox, "7b-breakdown.json", r#"{"op":"breakdown","project":"shop"}"#);

    let outcomes = poll(inbox, MAX_FILES_PER_TICK);
    match &outcomes[0] {
        InboxOutcome::Accepted { command, ack, .. } => {
            assert_eq!(*command, Command::Breakdown { project: "shop".to_string() });
            assert_eq!(ack, "queued");
        }
        other => panic!("expected Accepted, got {other:?}"),
    }
}

#[test]
fn approve_parses() {
    let tmp = TempDir::new().unwrap();
    let inbox = tmp.path();
    write_inbox_file(inbox, "7d-approve.json", r#"{"op":"approve","project":"shop"}"#);

    let outcomes = poll(inbox, MAX_FILES_PER_TICK);
    match &outcomes[0] {
        InboxOutcome::Accepted { command, ack, .. } => {
            assert_eq!(*command, Command::Approve { project: "shop".to_string() });
            assert_eq!(ack, "queued");
        }
        other => panic!("expected Accepted, got {other:?}"),
    }
}

#[test]
fn approve_missing_project_is_rejected() {
    let tmp = TempDir::new().unwrap();
    let inbox = tmp.path();
    write_inbox_file(inbox, "7e-approve-bad.json", r#"{"op":"approve"}"#);

    let outcomes = poll(inbox, MAX_FILES_PER_TICK);
    match &outcomes[0] {
        InboxOutcome::Rejected { reason, .. } => {
            assert!(reason.contains("project"), "reason: {reason}");
        }
        other => panic!("expected Rejected, got {other:?}"),
    }
}

#[test]
fn breakdown_missing_project_is_rejected() {
    let tmp = TempDir::new().unwrap();
    let inbox = tmp.path();
    write_inbox_file(inbox, "7c-breakdown-bad.json", r#"{"op":"breakdown"}"#);

    let outcomes = poll(inbox, MAX_FILES_PER_TICK);
    match &outcomes[0] {
        InboxOutcome::Rejected { reason, .. } => {
            assert!(reason.contains("project"), "reason: {reason}");
        }
        other => panic!("expected Rejected, got {other:?}"),
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

// ---------------------------------------------------------------------------
// Cross-crate pin: every office_core::inboxmsg builder must parse here
// ---------------------------------------------------------------------------
//
// This is the load-bearing test that welds `office-core`'s builders to this parser: each
// builder's output, serialized to a file, must parse into the exact `Command` the parser
// produces for a hand-written drop. If a field name drifts on EITHER side, this fails.

#[test]
fn every_inboxmsg_builder_roundtrips_through_parse() {
    use office_core::inboxmsg;

    let tmp = TempDir::new().unwrap();
    let inbox = tmp.path();

    // (filename in sort order, builder output, expected parsed Command). Filenames are
    // fixed-width so lexicographic == listed order, letting us zip sorted outcomes below.
    let cases: Vec<(&str, serde_json::Value, Command)> = vec![
        (
            "00-brief-proj.json",
            inboxmsg::brief(Some("shop"), "add a cart"),
            Command::Brief {
                project: Some("shop".to_string()),
                message: "add a cart".to_string(),
            },
        ),
        (
            "01-brief-none.json",
            inboxmsg::brief(None, "hello"),
            Command::Brief {
                project: None,
                message: "hello".to_string(),
            },
        ),
        (
            "02-status-proj.json",
            inboxmsg::status(Some("shop")),
            Command::Status {
                project: Some("shop".to_string()),
            },
        ),
        (
            "03-status-none.json",
            inboxmsg::status(None),
            Command::Status { project: None },
        ),
        (
            "04-authorize.json",
            inboxmsg::authorize("shop", "/tmp/out"),
            Command::Authorize {
                project: "shop".to_string(),
                delivery_path: "/tmp/out".to_string(),
            },
        ),
        (
            "05-comment.json",
            inboxmsg::comment("shop/e1/s1/t1", "looks good"),
            Command::Comment {
                task: "shop/e1/s1/t1".to_string(),
                text: "looks good".to_string(),
            },
        ),
        (
            "06-interrupt-hard.json",
            inboxmsg::interrupt("shop", true),
            Command::Interrupt {
                project: "shop".to_string(),
                hard: true,
            },
        ),
        (
            "07-interrupt-soft.json",
            inboxmsg::interrupt("shop", false),
            Command::Interrupt {
                project: "shop".to_string(),
                hard: false,
            },
        ),
        (
            "08-resume.json",
            inboxmsg::resume("shop"),
            Command::Resume {
                project: "shop".to_string(),
            },
        ),
        (
            "09-breakdown.json",
            inboxmsg::breakdown("shop"),
            Command::Breakdown {
                project: "shop".to_string(),
            },
        ),
        (
            "10-approve.json",
            inboxmsg::approve("shop"),
            Command::Approve {
                project: "shop".to_string(),
            },
        ),
    ];

    for (name, value, _expected) in &cases {
        write_inbox_file(inbox, name, &serde_json::to_string(value).unwrap());
    }

    let mut outcomes = poll(inbox, MAX_FILES_PER_TICK);
    outcomes.sort_by_key(|o| match o {
        InboxOutcome::Accepted { file, .. } => file.clone(),
        InboxOutcome::Rejected { file, .. } => file.clone(),
    });
    assert_eq!(outcomes.len(), cases.len(), "every builder must produce one accepted file");

    for ((name, _value, expected), outcome) in cases.iter().zip(outcomes.iter()) {
        match outcome {
            InboxOutcome::Accepted { file, command, .. } => {
                assert_eq!(file, name);
                assert_eq!(command, expected, "builder for {name} must roundtrip to its Command");
            }
            other => panic!("expected Accepted for {name}, got {other:?}"),
        }
    }
}

// ---------------------------------------------------------------------------
// Global inbox: peek_target + poll_global claim/leave/reject/race mechanics
// ---------------------------------------------------------------------------

#[test]
fn peek_target_classifies_every_op() {
    assert_eq!(
        peek_target(r#"{"op":"brief","project":"shop","message":"x"}"#),
        Target::Brief { project: Some("shop".to_string()) }
    );
    assert_eq!(
        peek_target(r#"{"op":"brief","message":"x"}"#),
        Target::Brief { project: None }
    );
    assert_eq!(
        peek_target(r#"{"op":"authorize","project":"shop","delivery_path":"/o"}"#),
        Target::Project { project: Some("shop".to_string()) }
    );
    assert_eq!(
        peek_target(r#"{"op":"resume","project":"shop"}"#),
        Target::Project { project: Some("shop".to_string()) }
    );
    assert_eq!(
        peek_target(r#"{"op":"status"}"#),
        Target::Project { project: None }
    );
    assert_eq!(
        peek_target(r#"{"op":"comment","task":"shop/e/s/t","text":"hi"}"#),
        Target::Task { task: "shop/e/s/t".to_string() }
    );
    assert_eq!(
        peek_target(r#"{"op":"breakdown","project":"shop"}"#),
        Target::Project { project: Some("shop".to_string()) }
    );
    assert_eq!(
        peek_target(r#"{"op":"approve","project":"shop"}"#),
        Target::Project { project: Some("shop".to_string()) }
    );
    // Undeterminable: bad JSON, unknown op, non-object, comment with no task.
    assert_eq!(peek_target("{ not json"), Target::Unknown);
    assert_eq!(peek_target(r#"{"op":"launch_missiles"}"#), Target::Unknown);
    assert_eq!(peek_target(r#"["op","brief"]"#), Target::Unknown);
    assert_eq!(peek_target(r#"{"op":"comment","text":"hi"}"#), Target::Unknown);
}

#[test]
fn poll_global_claims_owned_and_leaves_unowned() {
    let tmp = TempDir::new().unwrap();
    let inbox = tmp.path();
    write_inbox_file(inbox, "1-own.json", r#"{"op":"resume","project":"mine"}"#);
    write_inbox_file(inbox, "2-other.json", r#"{"op":"resume","project":"other"}"#);

    // Own only "mine".
    let owns = |t: &Target| match t {
        Target::Project { project: Some(p) } if p == "mine" => Claim::Claim,
        _ => Claim::Leave,
    };
    let outcomes = poll_global(inbox, MAX_FILES_PER_TICK, owns);

    assert_eq!(outcomes.len(), 1, "only the owned file is claimed");
    match &outcomes[0] {
        InboxOutcome::Accepted { file, command, .. } => {
            assert_eq!(file, "1-own.json");
            assert_eq!(*command, Command::Resume { project: "mine".to_string() });
        }
        other => panic!("expected Accepted, got {other:?}"),
    }

    // Owned file consumed into processed/, unowned file untouched in place.
    assert!(inbox.join("processed").join("1-own.json").exists());
    assert!(!inbox.join("1-own.json").exists());
    assert!(inbox.join("2-other.json").exists());
    assert!(!inbox.join("processed").join("2-other.json").exists());
}

#[test]
fn poll_global_rejects_malformed_claimable_file() {
    let tmp = TempDir::new().unwrap();
    let inbox = tmp.path();
    // authorize is missing its required delivery_path -> parse rejects. The project id
    // "mine" is present and (per the predicate) owned, so this instance may reject it.
    write_inbox_file(inbox, "1-bad.json", r#"{"op":"authorize","project":"mine"}"#);

    let outcomes = poll_global(inbox, MAX_FILES_PER_TICK, |_| Claim::Claim);
    assert_eq!(outcomes.len(), 1);
    match &outcomes[0] {
        InboxOutcome::Rejected { file, reason } => {
            assert_eq!(file, "1-bad.json");
            assert!(reason.contains("delivery_path"), "reason: {reason}");
        }
        other => panic!("expected Rejected, got {other:?}"),
    }
    assert!(inbox.join("rejected").join("1-bad.json").exists());
    assert!(inbox.join("rejected").join("1-bad.json.error").exists());
}

#[test]
fn poll_global_rename_race_loser_is_silent() {
    let tmp = TempDir::new().unwrap();
    let inbox = tmp.path();
    write_inbox_file(inbox, "1-own.json", r#"{"op":"resume","project":"mine"}"#);
    // Simulate a racing winner already occupying the destination slot: pre-create
    // processed/1-own.json as a DIRECTORY so this instance's claim rename fails.
    fs::create_dir_all(inbox.join("processed").join("1-own.json")).unwrap();

    let outcomes = poll_global(inbox, MAX_FILES_PER_TICK, |_| Claim::Claim);

    // Lost the race: no outcome, no panic, source file left untouched.
    assert!(outcomes.is_empty(), "the race loser emits no outcome");
    assert!(inbox.join("1-own.json").exists(), "the source file is left in place");
}

#[test]
fn poll_global_no_dir_yields_nothing() {
    let tmp = TempDir::new().unwrap();
    let missing = tmp.path().join("no-inbox");
    let outcomes = poll_global(&missing, MAX_FILES_PER_TICK, |_| Claim::Claim);
    assert!(outcomes.is_empty());
}
