#[cfg(test)]
mod tests {
    use crate::domain::CommentId;
    use crate::report::{parse_report, parse_review, ReportStatus, Verdict};

    #[test]
    fn parses_clean_complete_report() {
        let text = "Did the work.\n\nOFFICE-REPORT\nstatus: complete\nsummary: added retry logic\ndelivered: /deliver/fetcher.rs\nack-comments: c17,c18\n";
        let r = parse_report(text);
        assert_eq!(r.status, ReportStatus::Complete);
        assert_eq!(r.summary.as_deref(), Some("added retry logic"));
        assert_eq!(r.delivered, vec!["/deliver/fetcher.rs".to_string()]);
        assert_eq!(r.ack_comments, vec![CommentId(17), CommentId(18)]);
        assert_eq!(r.blocked_reason, None);
    }

    #[test]
    fn prose_after_the_block_is_ignored() {
        let text = "OFFICE-REPORT\nstatus: complete\nsummary: done\n\nThanks for reading, have a nice day!";
        let r = parse_report(text);
        assert_eq!(r.status, ReportStatus::Complete);
        assert_eq!(r.summary.as_deref(), Some("done"));
    }

    #[test]
    fn markdown_fenced_block_still_parses() {
        let text = "```\nOFFICE-REPORT\nstatus: complete\nsummary: fenced report\ndelivered: /deliver/a.rs\n```\n";
        let r = parse_report(text);
        assert_eq!(r.status, ReportStatus::Complete);
        assert_eq!(r.summary.as_deref(), Some("fenced report"));
        assert_eq!(r.delivered, vec!["/deliver/a.rs".to_string()]);
    }

    #[test]
    fn uppercase_key_and_marker_drift_still_parses() {
        let text = "office-report\nSTATUS: Complete\nSummary: caps everywhere\n";
        let r = parse_report(text);
        assert_eq!(r.status, ReportStatus::Complete);
        assert_eq!(r.summary.as_deref(), Some("caps everywhere"));
    }

    #[test]
    fn duplicate_blocks_last_one_wins() {
        let text = "OFFICE-REPORT\nstatus: blocked\nsummary: first attempt notes\nblocked-reason: need creds\n\nOFFICE-REPORT\nstatus: complete\nsummary: second block wins\n";
        let r = parse_report(text);
        assert_eq!(r.status, ReportStatus::Complete);
        assert_eq!(r.summary.as_deref(), Some("second block wins"));
        assert_eq!(r.blocked_reason, None);
    }

    #[test]
    fn missing_block_is_unparseable() {
        let r = parse_report("just some prose, no trailer at all");
        assert_eq!(r.status, ReportStatus::Unparseable);
        assert_eq!(r.summary, None);
        assert!(r.delivered.is_empty());
        assert!(r.ack_comments.is_empty());
    }

    #[test]
    fn blocked_status_captures_blocked_reason() {
        let text = "OFFICE-REPORT\nstatus: blocked\nsummary: could not proceed\nblocked-reason: need a decision on schema\n";
        let r = parse_report(text);
        assert_eq!(r.status, ReportStatus::Blocked);
        assert_eq!(r.blocked_reason.as_deref(), Some("need a decision on schema"));
    }

    #[test]
    fn ack_comments_list_parsed_with_c_prefix() {
        let text = "OFFICE-REPORT\nstatus: complete\nack-comments: c1, c22,c333\n";
        let r = parse_report(text);
        assert_eq!(r.ack_comments, vec![CommentId(1), CommentId(22), CommentId(333)]);
    }

    #[test]
    fn multiline_delivered_field_splits_into_paths() {
        let text = "OFFICE-REPORT\nstatus: complete\ndelivered: /deliver/a.rs\n/deliver/b.rs\n/deliver/c.rs\n";
        let r = parse_report(text);
        assert_eq!(
            r.delivered,
            vec![
                "/deliver/a.rs".to_string(),
                "/deliver/b.rs".to_string(),
                "/deliver/c.rs".to_string(),
            ]
        );
    }

    #[test]
    fn unrecognized_status_value_is_unparseable_but_other_fields_survive() {
        let text = "OFFICE-REPORT\nstatus: maybe\nsummary: unsure what happened\n";
        let r = parse_report(text);
        assert_eq!(r.status, ReportStatus::Unparseable);
        assert_eq!(r.summary.as_deref(), Some("unsure what happened"));
    }

    // --- OFFICE-REVIEW ---------------------------------------------------

    #[test]
    fn parses_clean_pass_review() {
        let text = "OFFICE-REVIEW\nverdict: pass\n";
        let r = parse_review(text);
        assert_eq!(r.verdict, Verdict::Pass);
        assert_eq!(r.reasons, None);
    }

    #[test]
    fn parses_fail_review_with_numbered_reasons() {
        let text = "OFFICE-REVIEW\nverdict: fail\nreasons: 1. retries not exponential\n2. no tests added\n";
        let r = parse_review(text);
        assert_eq!(r.verdict, Verdict::Fail);
        assert_eq!(
            r.reasons.as_deref(),
            Some("1. retries not exponential\n2. no tests added")
        );
    }

    #[test]
    fn review_missing_block_is_unparseable() {
        let r = parse_review("the reviewer forgot the trailer entirely");
        assert_eq!(r.verdict, Verdict::Unparseable);
    }

    #[test]
    fn review_duplicate_blocks_last_wins() {
        let text = "OFFICE-REVIEW\nverdict: fail\nreasons: bad\n\nOFFICE-REVIEW\nverdict: pass\n";
        let r = parse_review(text);
        assert_eq!(r.verdict, Verdict::Pass);
        assert_eq!(r.reasons, None);
    }

    #[test]
    fn review_case_and_fence_tolerant() {
        let text = "```\noffice-review\nVERDICT: Fail\nREASONS: strict is strict\n```\n";
        let r = parse_review(text);
        assert_eq!(r.verdict, Verdict::Fail);
        assert_eq!(r.reasons.as_deref(), Some("strict is strict"));
    }

    // --- rolling-score hygiene grade (item 3) ----------------------------

    #[test]
    fn review_parses_optional_hygiene_grade() {
        let text = "OFFICE-REVIEW\nverdict: pass\nreasons: clean\nhygiene: 85\n";
        let r = parse_review(text);
        assert_eq!(r.verdict, Verdict::Pass);
        assert_eq!(r.hygiene, Some(85));
    }

    #[test]
    fn review_hygiene_absent_is_none_for_compat() {
        // An older reviewer that omits the line parses fine; the kernel treats None as 100.
        let r = parse_review("OFFICE-REVIEW\nverdict: pass\n");
        assert_eq!(r.hygiene, None);
    }

    #[test]
    fn review_hygiene_tolerates_slash_and_clamps() {
        // First digit run wins (`92/100` -> 92); values are clamped to 100.
        assert_eq!(parse_review("OFFICE-REVIEW\nverdict: pass\nhygiene: 92/100\n").hygiene, Some(92));
        assert_eq!(parse_review("OFFICE-REVIEW\nverdict: pass\nhygiene: 150\n").hygiene, Some(100));
    }

    // --- OFFICE-AUDIT (6.2c) ---------------------------------------------

    use crate::report::{parse_assume_check, parse_audit, AssumeVerdict};

    #[test]
    fn parses_audit_grade_and_failures() {
        let text = "I inspected the tree.\nOFFICE-AUDIT\ngrade: 72\nfailures:\n- module utils.rs is unwired\n- debug prints left in main.rs\n";
        let r = parse_audit(text);
        assert_eq!(r.grade, Some(72));
        assert_eq!(
            r.failures,
            vec!["module utils.rs is unwired".to_string(), "debug prints left in main.rs".to_string()]
        );
    }

    #[test]
    fn audit_grade_tolerates_slash_and_prose_and_clamps() {
        assert_eq!(parse_audit("OFFICE-AUDIT\ngrade: 87/100\n").grade, Some(87));
        assert_eq!(parse_audit("OFFICE-AUDIT\ngrade: 95 (pass)\n").grade, Some(95));
        assert_eq!(parse_audit("OFFICE-AUDIT\ngrade: 250\n").grade, Some(100), "clamped to 100");
    }

    #[test]
    fn audit_missing_block_or_grade_is_inconclusive() {
        assert_eq!(parse_audit("no block here").grade, None);
        assert!(parse_audit("no block here").failures.is_empty());
        // Block present but no numeric grade -> inconclusive (None), fail-open in the kernel.
        assert_eq!(parse_audit("OFFICE-AUDIT\ngrade: pending\n").grade, None);
    }

    #[test]
    fn audit_case_and_fence_tolerant_pass_with_no_failures() {
        let text = "```\noffice-audit\nGRADE: 100\n```\n";
        let r = parse_audit(text);
        assert_eq!(r.grade, Some(100));
        assert!(r.failures.is_empty());
    }

    // --- ASSUME-CHECK (6.2c) ---------------------------------------------

    #[test]
    fn parses_assume_check_clean() {
        let c = parse_assume_check("Looks grounded.\nASSUME-CHECK\nverdict: clean\n").unwrap();
        assert_eq!(c.verdict, AssumeVerdict::Clean);
        assert!(c.items.is_empty());
    }

    #[test]
    fn parses_assume_check_assumptions_with_items() {
        // The bare `- item` lines fold into the open verdict key; the parser peels the first
        // (verdict word) from the rest (items).
        let text = "ASSUME-CHECK\nverdict: assumptions\n- assumed Postgres, user never said\n- picked React, not stated\n";
        let c = parse_assume_check(text).unwrap();
        assert_eq!(c.verdict, AssumeVerdict::Assumptions);
        assert_eq!(
            c.items,
            vec!["assumed Postgres, user never said".to_string(), "picked React, not stated".to_string()]
        );
    }

    #[test]
    fn assume_check_missing_block_is_none() {
        assert!(parse_assume_check("the safeguard forgot the block").is_none());
    }

    #[test]
    fn assume_check_case_and_fence_tolerant() {
        let c = parse_assume_check("```\nassume-check\nVERDICT: Assumptions\n- x\n```\n").unwrap();
        assert_eq!(c.verdict, AssumeVerdict::Assumptions);
        assert_eq!(c.items, vec!["x".to_string()]);
    }

    // --- criticality classification (autonomous-safeguard pivot) --------

    use crate::report::classify_assumption;

    #[test]
    fn classify_tags_critical_and_strips_the_tag() {
        let c = classify_assumption("[critical] spends real money on a paid API");
        assert!(c.critical);
        assert_eq!(c.text, "spends real money on a paid API");
    }

    #[test]
    fn classify_tags_auto_and_strips_the_tag() {
        let c = classify_assumption("[auto] uses Postgres for storage");
        assert!(!c.critical);
        assert_eq!(c.text, "uses Postgres for storage");
    }

    #[test]
    fn classify_untagged_defaults_to_auto() {
        // An untagged item is the safe default: the office decides it, no human freeze.
        let c = classify_assumption("picked React, not stated");
        assert!(!c.critical);
        assert_eq!(c.text, "picked React, not stated");
    }

    #[test]
    fn classify_is_case_insensitive_and_whitespace_tolerant() {
        let c = classify_assumption("  [CRITICAL]  deploys to production");
        assert!(c.critical);
        assert_eq!(c.text, "deploys to production");
        let a = classify_assumption("[Auto]: chooses a folder layout");
        assert!(!a.critical);
        assert_eq!(a.text, "chooses a folder layout");
    }

    #[test]
    fn classify_bare_tag_yields_empty_text() {
        // A bare tag with no item text -> empty text (the caller drops it).
        assert_eq!(classify_assumption("[critical]").text, "");
        assert_eq!(classify_assumption("[auto]").text, "");
    }

    // --- SDLC-TRIAGE (feature: sdlc-triage) ------------------------------

    use crate::report::{parse_triage, TriageTrack};

    #[test]
    fn parses_triage_project() {
        let v = parse_triage("SDLC-TRIAGE\ntrack: project\nrationale: a new build\nexisting: no\n");
        assert_eq!(v.track, TriageTrack::Project);
        assert_eq!(v.rationale, "a new build");
        assert!(!v.existing);
    }

    #[test]
    fn parses_triage_enhancement_targeting_existing() {
        let v = parse_triage(
            "SDLC-TRIAGE\ntrack: enhancement\nrationale: add a filter to the existing list\nexisting: yes\n",
        );
        assert_eq!(v.track, TriageTrack::Enhancement);
        assert_eq!(v.rationale, "add a filter to the existing list");
        assert!(v.existing, "targets an existing delivery");
    }

    #[test]
    fn parses_triage_patch() {
        // No `existing:` line -> defaults false; a leading-prose reply is tolerated.
        let v = parse_triage("Here is my call.\nSDLC-TRIAGE\ntrack: patch\nrationale: fix a typo\n");
        assert_eq!(v.track, TriageTrack::Patch);
        assert!(!v.existing, "absent existing line defaults to false");
    }

    #[test]
    fn triage_garbage_defaults_to_project() {
        // Missing block, unknown track word, and empty input ALL default to the safe project track.
        assert_eq!(parse_triage("the classifier forgot the block").track, TriageTrack::Project);
        assert_eq!(parse_triage("SDLC-TRIAGE\ntrack: banana\nrationale: ?\n").track, TriageTrack::Project);
        assert_eq!(parse_triage("").track, TriageTrack::Project);
    }

    #[test]
    fn triage_case_and_fence_tolerant() {
        let v = parse_triage("```\nsdlc-triage\nTRACK: Enhancement\nrationale: scoped change\n```\n");
        assert_eq!(v.track, TriageTrack::Enhancement);
    }

    // -----------------------------------------------------------------------
    // SPRINT-REVIEW (feature: sprints)
    // -----------------------------------------------------------------------
    use crate::report::{parse_sprint_review, SprintAdjustmentKind};

    #[test]
    fn parse_sprint_review_extracts_summary_and_adjustments() {
        let text = "prose\nSPRINT-REVIEW\nsummary: shipped the http client; retry carried over because the API was flaky\nadjustments:\n- drop old-task\n- add caching layer | add an LRU cache in front of the client\n- modify retry-task | use exponential backoff\n";
        let plan = parse_sprint_review(text);
        assert!(plan.summary.contains("shipped the http client"));
        assert_eq!(plan.adjustments.len(), 3);
        assert_eq!(plan.adjustments[0].kind, SprintAdjustmentKind::Drop);
        assert_eq!(plan.adjustments[0].target, "old-task");
        assert_eq!(plan.adjustments[1].kind, SprintAdjustmentKind::Add);
        assert_eq!(plan.adjustments[1].target, "caching layer");
        assert_eq!(plan.adjustments[1].text, "add an LRU cache in front of the client");
        assert_eq!(plan.adjustments[2].kind, SprintAdjustmentKind::Modify);
        assert_eq!(plan.adjustments[2].target, "retry-task");
        assert_eq!(plan.adjustments[2].text, "use exponential backoff");
    }

    #[test]
    fn parse_sprint_review_garbage_yields_empty() {
        // No SPRINT-REVIEW block at all -> default (empty), so the ceremony carries over only.
        let plan = parse_sprint_review("the office rambled but emitted no block");
        assert!(plan.summary.is_empty());
        assert!(plan.adjustments.is_empty());
    }

    #[test]
    fn parse_sprint_review_ignores_unknown_verbs_and_empty_targets() {
        let text = "SPRINT-REVIEW\nsummary: ok\nadjustments:\n- frobnicate the widget\n- drop\n- add | only a description\n";
        let plan = parse_sprint_review(text);
        // 'frobnicate' is not a verb; bare 'drop' has no target; 'add | ..' has an empty title.
        assert!(plan.adjustments.is_empty(), "got {:?}", plan.adjustments);
    }
}
