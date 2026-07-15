//! Thin, injectable `git` subprocess wrapper for worktree desks (item 1).
//!
//! Every operation shells out to `git` with an explicit `-C <path>` so the working directory is
//! never assumed, is best-effort + NEVER panics, and distinguishes two failure classes:
//!   - [`GitError::NotFound`] — the `git` binary could not be spawned (not on PATH). The driver maps
//!     this to the graceful fallback: the project runs legacy copy-desks.
//!   - [`GitError::Failed`] — `git` ran but exited non-zero. The driver maps this to a task
//!     bounce/degrade, tracing the stderr; it never wedges the line.
//!
//! The binary path is INJECTABLE ([`Git::new`]) so tests can point it at a nonexistent path (to
//! exercise the fallback) or a stub without a real repo. Serialization "one git at a time per repo"
//! is enforced by the driver (a per-repo mutex around each call site), not here.

use std::path::Path;
use std::process::Command;

/// Why a `git` invocation did not succeed.
#[derive(Debug)]
pub enum GitError {
    /// The `git` binary itself could not be spawned (not on PATH). Triggers the legacy fallback.
    NotFound(String),
    /// `git` ran but exited non-zero; carries the trimmed stderr (or a synthesized message).
    Failed(String),
}

impl GitError {
    /// A short human reason for a trace / bounce note.
    pub fn reason(&self) -> String {
        match self {
            GitError::NotFound(e) => format!("git not on PATH: {e}"),
            GitError::Failed(e) => e.clone(),
        }
    }
}

/// The outcome of a branch merge into main.
#[derive(Debug, PartialEq, Eq)]
pub enum MergeOutcome {
    /// Merged cleanly (fast-forward or a merge commit).
    Merged,
    /// The merge could not complete cleanly; carries a conflict/error summary. The working tree is
    /// left clean (any partial merge is aborted) so main is never corrupted.
    Conflict(String),
}

/// An injectable handle to the `git` binary. Cheap to clone.
#[derive(Clone, Debug)]
pub struct Git {
    bin: String,
}

impl Git {
    /// `bin` is the git executable to invoke (`"git"` in production; a nonexistent / stub path in
    /// tests).
    pub fn new(bin: impl Into<String>) -> Self {
        Git { bin: bin.into() }
    }

    /// Run `git -C <cwd> <args...>`; `Ok(stdout)` on exit 0, else a classified [`GitError`].
    fn run(&self, cwd: &Path, args: &[&str]) -> Result<String, GitError> {
        let out = Command::new(&self.bin)
            .arg("-C")
            .arg(cwd)
            .args(args)
            .output()
            .map_err(|e| GitError::NotFound(e.to_string()))?;
        if out.status.success() {
            Ok(String::from_utf8_lossy(&out.stdout).into_owned())
        } else {
            let err = String::from_utf8_lossy(&out.stderr).trim().to_string();
            Err(GitError::Failed(if err.is_empty() {
                format!("git {} exited non-zero", args.join(" "))
            } else {
                err
            }))
        }
    }

    /// Whether `delivery` is already a git repo (has a `.git` entry): respect it untouched. A plain
    /// filesystem check, so it works even when `git` is missing (the later ops then surface that).
    pub fn is_repo(&self, delivery: &Path) -> bool {
        delivery.join(".git").exists()
    }

    /// `git init` `delivery` on branch `main`, configure a local identity so `commit` never prompts
    /// or errors in a clean environment, then stage + make ONE (allow-empty) initial commit of
    /// whatever exists so the repo has a HEAD to branch worktrees from (item 1). Only called when
    /// `!is_repo`.
    pub fn init_repo(&self, delivery: &Path) -> Result<(), GitError> {
        self.run(delivery, &["init"])?;
        // Force the default branch to `main` regardless of the host git's default, pre-first-commit
        // (works on every git version, unlike `init -b main`).
        let _ = self.run(delivery, &["symbolic-ref", "HEAD", "refs/heads/main"]);
        // Local identity + no signing so the initial commit is non-interactive and deterministic.
        let _ = self.run(delivery, &["config", "user.email", "office@koma-workflow.local"]);
        let _ = self.run(delivery, &["config", "user.name", "Workflow Office"]);
        let _ = self.run(delivery, &["config", "commit.gpgsign", "false"]);
        self.run(delivery, &["add", "-A"])?;
        self.run(delivery, &["commit", "--allow-empty", "-m", "workflow: initial delivery snapshot"])?;
        Ok(())
    }

    /// The repo's current (main) branch name — the branch the main worktree stays checked out on,
    /// which every task worktree branches from and every merge merges into.
    pub fn main_branch(&self, repo: &Path) -> Result<String, GitError> {
        Ok(self.run(repo, &["rev-parse", "--abbrev-ref", "HEAD"])?.trim().to_string())
    }

    /// Whether the repo has a resolvable HEAD commit — worktrees can only branch off a real commit,
    /// so an existing-but-empty repo (unborn HEAD) must fall back to legacy desks (item 1). Also the
    /// `git`-availability probe: a missing binary makes this `false`.
    pub fn has_head_commit(&self, repo: &Path) -> bool {
        self.run(repo, &["rev-parse", "--verify", "HEAD"]).is_ok()
    }

    /// Materialize a FRESH worktree for `branch` at `desk`, branched off the current main tip (item
    /// 1). Any stale worktree/branch of the same name is torn down first, so a retry always gets a
    /// clean tree.
    pub fn add_worktree(&self, repo: &Path, desk: &Path, branch: &str) -> Result<(), GitError> {
        self.remove_worktree(repo, desk, branch);
        let main = self.main_branch(repo)?;
        let desk_s = desk.to_string_lossy();
        self.run(repo, &["worktree", "add", "-b", branch, &desk_s, &main])?;
        Ok(())
    }

    /// Stage + commit everything in the worktree at `desk` onto its task branch (item 2). "Nothing
    /// to commit" is a legitimate empty delivery, NOT an error (the reviewer then fails the empty
    /// diff) — detected via `status --porcelain` since `git commit` prints that notice to STDOUT and
    /// still exits non-zero. Any real failure surfaces.
    pub fn commit_all(&self, desk: &Path, msg: &str) -> Result<(), GitError> {
        self.run(desk, &["add", "-A"])?;
        if self.run(desk, &["status", "--porcelain"])?.trim().is_empty() {
            return Ok(()); // nothing staged: an empty delivery, not an error
        }
        self.run(desk, &["commit", "-m", msg])?;
        Ok(())
    }

    /// The `git diff <main>...<branch> --stat` output (item 2) computed from the worktree — the
    /// changes this task introduces relative to where it diverged from main. Best-effort: an error
    /// yields an empty string (the reviewer still inspects the tree directly).
    pub fn diff_stat(&self, repo: &Path, desk: &Path, branch: &str) -> String {
        let main = match self.main_branch(repo) {
            Ok(m) => m,
            Err(_) => return String::new(),
        };
        let range = format!("{main}...{branch}");
        self.run(desk, &["diff", &range, "--stat"]).unwrap_or_default()
    }

    /// Merge `branch` into the repo's main branch, fast-forward preferred (item 1). A clean merge
    /// -> [`MergeOutcome::Merged`]; anything else (conflict or error) aborts any partial merge and
    /// returns [`MergeOutcome::Conflict`] with a summary of the conflicting files.
    pub fn merge(&self, repo: &Path, branch: &str) -> MergeOutcome {
        match self.run(repo, &["merge", "--no-edit", branch]) {
            Ok(_) => MergeOutcome::Merged,
            Err(e) => {
                let conflicts = self
                    .run(repo, &["diff", "--name-only", "--diff-filter=U"])
                    .unwrap_or_default();
                // Leave main clean: abort any half-applied merge (ignored if there was no merge in
                // progress, e.g. a non-conflict error).
                let _ = self.run(repo, &["merge", "--abort"]);
                let files: Vec<&str> = conflicts.split_whitespace().collect();
                let summary = if files.is_empty() {
                    e.reason()
                } else {
                    format!("conflict in {}", files.join(", "))
                };
                MergeOutcome::Conflict(summary)
            }
        }
    }

    /// Remove a task's worktree + delete its branch (item 1). Best-effort and idempotent: each step
    /// is ignored on failure, and the desk dir is scrubbed even if it was never a registered
    /// worktree, so a stale/aborted setup never blocks a fresh one.
    pub fn remove_worktree(&self, repo: &Path, desk: &Path, branch: &str) {
        let desk_s = desk.to_string_lossy();
        let _ = self.run(repo, &["worktree", "remove", "--force", &desk_s]);
        let _ = std::fs::remove_dir_all(desk);
        let _ = self.run(repo, &["worktree", "prune"]);
        let _ = self.run(repo, &["branch", "-D", branch]);
    }
}

#[cfg(test)]
#[path = "git_test.rs"]
mod git_test;
