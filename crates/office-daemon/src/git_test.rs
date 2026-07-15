//! Tests for the `git` worktree wrapper (item 1), against a REAL `git` in a tempdir. Each test
//! `git init`s a throwaway repo so nothing touches the developer's tree.

use super::{Git, GitError, MergeOutcome};
use std::fs;
use std::path::Path;

fn git() -> Git {
    Git::new("git")
}

fn write(path: &Path, name: &str, body: &str) {
    fs::write(path.join(name), body).expect("write file");
}

fn read(path: &Path, name: &str) -> String {
    fs::read_to_string(path.join(name)).unwrap_or_default()
}

#[test]
fn init_repo_makes_a_committable_main_repo() {
    let dir = tempfile::tempdir().unwrap();
    let repo = dir.path();
    let g = git();
    assert!(!g.is_repo(repo), "empty dir is not a repo yet");
    g.init_repo(repo).expect("init");
    assert!(g.is_repo(repo), ".git exists after init");
    // Forced onto `main`, with an initial (possibly empty) commit as HEAD.
    assert_eq!(g.main_branch(repo).unwrap(), "main");
}

#[test]
fn init_repo_respects_existing_content_in_the_first_commit() {
    let dir = tempfile::tempdir().unwrap();
    let repo = dir.path();
    write(repo, "seed.txt", "already here");
    git().init_repo(repo).expect("init");
    // The seed file is captured by the initial commit (a worktree branched off main sees it).
    let dir2 = tempfile::tempdir().unwrap();
    let desk = dir2.path().join("wt");
    git().add_worktree(repo, &desk, "task/x").expect("worktree");
    assert_eq!(read(&desk, "seed.txt"), "already here");
}

#[test]
fn add_worktree_is_a_fresh_branch_checkout() {
    let dir = tempfile::tempdir().unwrap();
    let repo = dir.path();
    let g = git();
    g.init_repo(repo).unwrap();
    let deskdir = tempfile::tempdir().unwrap();
    let desk = deskdir.path().join("t1");
    g.add_worktree(repo, &desk, "task/t1").expect("add");
    assert!(desk.exists(), "worktree dir materialized");
    // A second add on the SAME branch/desk tears down the stale one and re-creates it cleanly.
    g.add_worktree(repo, &desk, "task/t1").expect("re-add is fresh");
    assert!(desk.exists());
}

#[test]
fn commit_all_then_diff_stat_shows_the_change() {
    let dir = tempfile::tempdir().unwrap();
    let repo = dir.path();
    let g = git();
    g.init_repo(repo).unwrap();
    let deskdir = tempfile::tempdir().unwrap();
    let desk = deskdir.path().join("t1");
    g.add_worktree(repo, &desk, "task/t1").unwrap();
    write(&desk, "feature.rs", "fn feature() {}\n");
    g.commit_all(&desk, "workflow: task t1").expect("commit");
    let stat = g.diff_stat(repo, &desk, "task/t1");
    assert!(stat.contains("feature.rs"), "diff-stat names the changed file: {stat}");
}

#[test]
fn commit_all_tolerates_an_empty_delivery() {
    let dir = tempfile::tempdir().unwrap();
    let repo = dir.path();
    let g = git();
    g.init_repo(repo).unwrap();
    let deskdir = tempfile::tempdir().unwrap();
    let desk = deskdir.path().join("t1");
    g.add_worktree(repo, &desk, "task/t1").unwrap();
    // Worker delivered nothing: `nothing to commit` is Ok(()), not an error.
    g.commit_all(&desk, "workflow: task t1").expect("empty commit is fine");
}

#[test]
fn merge_clean_advances_main() {
    let dir = tempfile::tempdir().unwrap();
    let repo = dir.path();
    let g = git();
    g.init_repo(repo).unwrap();
    let deskdir = tempfile::tempdir().unwrap();
    let desk = deskdir.path().join("t1");
    g.add_worktree(repo, &desk, "task/t1").unwrap();
    write(&desk, "added.rs", "// from t1\n");
    g.commit_all(&desk, "workflow: task t1").unwrap();
    assert_eq!(g.merge(repo, "task/t1"), MergeOutcome::Merged);
    // The merged file is now on main (the delivery tree).
    assert_eq!(read(repo, "added.rs"), "// from t1\n");
}

#[test]
fn merge_conflict_is_reported_and_leaves_main_clean() {
    let dir = tempfile::tempdir().unwrap();
    let repo = dir.path();
    let g = git();
    write(repo, "shared.txt", "base\n");
    g.init_repo(repo).unwrap();

    // Task branch edits shared.txt.
    let deskdir = tempfile::tempdir().unwrap();
    let desk = deskdir.path().join("t1");
    g.add_worktree(repo, &desk, "task/t1").unwrap();
    write(&desk, "shared.txt", "task-side change\n");
    g.commit_all(&desk, "workflow: task t1").unwrap();

    // Main diverges on the SAME line after the branch was taken.
    write(repo, "shared.txt", "main-side change\n");
    g.commit_all(repo, "main advances").unwrap();

    match g.merge(repo, "task/t1") {
        MergeOutcome::Conflict(summary) => assert!(
            summary.contains("shared.txt"),
            "conflict summary names the file: {summary}"
        ),
        other => panic!("expected a conflict, got {other:?}"),
    }
    // Main is left clean (the aborted merge did not corrupt it): still the main-side content.
    assert_eq!(read(repo, "shared.txt"), "main-side change\n");
}

#[test]
fn merge_non_conflict_failure_is_reported_as_failed_not_conflict() {
    // item 4: a merge failure with NO conflicted files (branch doesn't exist) must not be reported
    // as a Conflict — that would misleadingly tell the caller to "resolve the conflict".
    let dir = tempfile::tempdir().unwrap();
    let repo = dir.path();
    let g = git();
    g.init_repo(repo).unwrap();

    match g.merge(repo, "task/does-not-exist") {
        MergeOutcome::Failed(summary) => assert!(!summary.is_empty(), "failure carries a reason"),
        other => panic!("expected Failed, got {other:?}"),
    }
}

#[test]
fn commit_and_merge_succeed_without_any_configured_identity() {
    // item 2: an ADOPTED repo (raw `git init`, no local identity — unlike our `init_repo`) must
    // still be committable/mergeable. HOME points at an empty tempdir so no real `~/.gitconfig`
    // leaks an identity in either — without the explicit `-c user.email=.../user.name=...` on the
    // commit-creating calls, this would fail with "committer identity unknown".
    let empty_home = tempfile::tempdir().unwrap();
    let saved_home = std::env::var("HOME").ok();
    std::env::set_var("HOME", empty_home.path());

    let result = std::panic::catch_unwind(|| {
        let dir = tempfile::tempdir().unwrap();
        let repo = dir.path();
        let g = git();
        // Raw `git init`, NOT `g.init_repo` — no local user.email/user.name is configured.
        std::process::Command::new("git").arg("-C").arg(repo).arg("init").output().unwrap();
        std::process::Command::new("git")
            .arg("-C")
            .arg(repo)
            .args(["symbolic-ref", "HEAD", "refs/heads/main"])
            .output()
            .unwrap();
        write(repo, "seed.txt", "seed\n");
        g.commit_all(repo, "seed commit").expect("commit with no configured identity");

        let deskdir = tempfile::tempdir().unwrap();
        let desk = deskdir.path().join("t1");
        g.add_worktree(repo, &desk, "task/t1").expect("worktree");
        write(&desk, "added.rs", "// from t1\n");
        g.commit_all(&desk, "workflow: task t1").expect("desk commit with no configured identity");
        assert_eq!(g.merge(repo, "task/t1"), MergeOutcome::Merged, "merge with no configured identity");
    });

    match saved_home {
        Some(h) => std::env::set_var("HOME", h),
        None => std::env::remove_var("HOME"),
    }
    result.expect("test body panicked");
}

#[test]
fn remove_worktree_reclaims_dir_and_branch() {
    let dir = tempfile::tempdir().unwrap();
    let repo = dir.path();
    let g = git();
    g.init_repo(repo).unwrap();
    let deskdir = tempfile::tempdir().unwrap();
    let desk = deskdir.path().join("t1");
    g.add_worktree(repo, &desk, "task/t1").unwrap();
    assert!(desk.exists());
    g.remove_worktree(repo, &desk, "task/t1");
    assert!(!desk.exists(), "worktree dir removed");
    // The branch is gone: adding it again succeeds (would fail if the branch still existed and was
    // checked out elsewhere).
    g.add_worktree(repo, &desk, "task/t1").expect("re-add after remove");
}

#[test]
fn empty_existing_repo_has_no_head_commit_until_committed() {
    // An existing-but-empty repo (unborn HEAD) reports no HEAD commit, so the driver falls back to
    // legacy desks; after an initial commit it is worktree-capable.
    let dir = tempfile::tempdir().unwrap();
    let repo = dir.path();
    std::process::Command::new("git").arg("-C").arg(repo).arg("init").output().unwrap();
    let g = git();
    assert!(g.is_repo(repo), "init'd dir is a repo");
    assert!(!g.has_head_commit(repo), "no HEAD commit yet");
    g.init_repo(repo).unwrap();
    assert!(g.has_head_commit(repo), "committed => worktree-capable");
}

#[test]
fn missing_git_binary_is_not_found() {
    let dir = tempfile::tempdir().unwrap();
    let g = Git::new("/nonexistent/definitely-not-git");
    match g.init_repo(dir.path()) {
        Err(GitError::NotFound(_)) => {}
        other => panic!("expected NotFound, got {other:?}"),
    }
}
