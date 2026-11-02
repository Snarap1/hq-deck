//! Git access, split by concern.
//!
//! - [`scope`] — the review side: scope changesets, diffs, turn baselines, forge identity.
//!   Read-only apart from the private refs under `refs/worktree/reviewr/`.
//! - [`porcelain`] — the source-control side: `status --porcelain` parsing and the
//!   stage / unstage / commit / discard operations. Those do write to the index and the
//!   worktree, and only ever under the user's own gesture.
//! - [`drawers`] — display-only lists: branches, remotes, stashes, tags, worktrees, log.
//! - [`ops`] — the `Git ops` tab: update / push / fetch all / merge / rebase, table-driven and
//!   run only under the user's own confirmed gesture (plan M6).
//!
//! Every git subprocess in the crate starts from one of the runners below.

use std::path::Path;

use anyhow::{Context, Result, bail};

pub mod drawers;
pub mod ops;
pub mod porcelain;
mod scope;

pub use scope::*;

/// Run `git -C <repo> <args>` and return stdout. Errors on non-zero exit.
///
/// Public for the SCM context menu's one-off ops (checkout, merge, stash, tag, remote —
/// `scm_panel.rs`): those commands are menu-item-scoped, not a table-driven module of their own
/// like `git/ops.rs`, so they run through this shared entry rather than a second wrapper over
/// `crate::proc`.
pub fn run(repo: &Path, args: &[&str]) -> Result<String> {
    git(repo, args)
}

pub(super) fn git(repo: &Path, args: &[&str]) -> Result<String> {
    let out = crate::proc::command("git")
        .arg("-C")
        .arg(repo)
        .args(["-c", "core.quotepath=false"])
        .args(args)
        .output()
        .with_context(|| format!("running git {args:?}"))?;
    if !out.status.success() {
        bail!("git {args:?} failed: {}", String::from_utf8_lossy(&out.stderr).trim());
    }
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

/// Like [`git`], but returns stdout even on non-zero exit (e.g. `diff --no-index`).
pub(super) fn git_lenient(repo: &Path, args: &[&str]) -> String {
    crate::proc::command("git")
        .arg("-C")
        .arg(repo)
        .args(["-c", "core.quotepath=false"])
        .args(args)
        .output()
        .map(|o| String::from_utf8_lossy(&o.stdout).into_owned())
        .unwrap_or_default()
}

/// Run `git -C <repo> <args>` and return its trimmed stdout, or `None` if the command fails to
/// spawn, exits non-zero, or prints nothing. The one-line query workhorse for `rev-parse`/`merge-base`.
pub(super) fn git_line(repo: &Path, args: &[&str]) -> Option<String> {
    let out = crate::proc::command("git").arg("-C").arg(repo).args(args).output().ok()?;
    if !out.status.success() {
        return None;
    }
    let line = String::from_utf8_lossy(&out.stdout).trim().to_string();
    (!line.is_empty()).then_some(line)
}

/// Whether `git -C <repo> <args>` spawns and exits zero. The predicate workhorse for existence checks.
pub(super) fn git_ok(repo: &Path, args: &[&str]) -> bool {
    crate::proc::command("git")
        .arg("-C")
        .arg(repo)
        .args(args)
        .output()
        .is_ok_and(|o| o.status.success())
}
