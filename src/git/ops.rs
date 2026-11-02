//! The `Git ops` tab's repository-level operations: update, push, fetch all, merge into
//! current, rebase onto — argv construction and the background-safe runner. Pure like
//! [`super::porcelain`]/[`super::drawers`]; the panel state (`app.rs`) and its render
//! (`ui.rs`) are the ratatui-aware halves (plan M6, draft §4.3).

use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver};

use anyhow::Result;

use super::git;

/// One action the `Git ops` tab can run, exactly the draft §4.3 table.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Op {
    Update,
    Push,
    FetchAll,
    /// Bring the repository's main branch up to its `origin` tip from any branch.
    /// `checked_out` is whether that branch is the current one: then it is a plain
    /// [`Op::Update`], otherwise a fast-forward-only `fetch origin <b>:<b>` that never
    /// touches the worktree.
    UpdateMain {
        branch: String,
        checked_out: bool,
    },
    MergeInto(String),
    RebaseOnto(String),
}

impl Op {
    /// The action list's label, before a branch is picked for `MergeInto`/`RebaseOnto`.
    pub fn label(&self) -> &'static str {
        match self {
            Op::Update => "Update",
            Op::Push => "Push",
            Op::FetchAll => "Fetch all",
            Op::UpdateMain { .. } => "Update main branch",
            Op::MergeInto(_) => "Merge into current",
            Op::RebaseOnto(_) => "Rebase onto",
        }
    }

    /// The confirmation prompt shown before running — merge/rebase name the target branch
    /// against the current one, so the user confirms the exact direction (plan M6 step 4).
    pub fn confirm_message(&self, current_branch: &str) -> String {
        match self {
            Op::Update => "update: pull --rebase=false --autostash --prune?".to_string(),
            Op::Push => "push".to_string(),
            Op::FetchAll => "fetch --all --prune?".to_string(),
            Op::UpdateMain { branch, checked_out: true } => {
                format!("update `{branch}`: pull --rebase=false --autostash --prune?")
            }
            Op::UpdateMain { branch, checked_out: false } => {
                format!("fast-forward `{branch}` to `origin/{branch}`?")
            }
            Op::MergeInto(branch) => format!("merge `{branch}` into `{current_branch}`"),
            Op::RebaseOnto(branch) => format!("rebase `{current_branch}` onto `{branch}`"),
        }
    }

    /// The `git` argv this op runs, given whether the current branch already has an upstream
    /// (`Push` sets one with `-u origin HEAD` the first time, plan M6 step 2).
    fn argv(&self, has_upstream: bool) -> Vec<String> {
        match self {
            // A merge pull, spelled explicitly: `--rebase=false` overrides a `pull.rebase = true`
            // in the user's git config, where a bare `pull` would rebase behind their back. The
            // rebase that is asked for has its own row in this table. `--prune` drops remote
            // branches that are gone, as IntelliJ's Update does.
            Op::Update => {
                ["pull", "--rebase=false", "--autostash", "--prune"].map(String::from).to_vec()
            }
            Op::Push if has_upstream => vec!["push".to_string()],
            Op::Push => ["push", "-u", "origin", "HEAD"].map(String::from).to_vec(),
            Op::FetchAll => ["fetch", "--all", "--prune"].map(String::from).to_vec(),
            Op::UpdateMain { checked_out: true, .. } => Op::Update.argv(has_upstream),
            // `<b>:<b>` updates the local branch only on a fast-forward, and git refuses it
            // while the branch is checked out in another worktree - both refusals surface as
            // the op's error, never as a forced move.
            Op::UpdateMain { branch, checked_out: false } => vec![
                "fetch".to_string(),
                "origin".to_string(),
                "--prune".to_string(),
                format!("refs/heads/{branch}:refs/heads/{branch}"),
            ],
            Op::MergeInto(branch) => {
                vec!["merge".to_string(), "--no-edit".to_string(), branch.clone()]
            }
            Op::RebaseOnto(branch) => vec!["rebase".to_string(), branch.clone()],
        }
    }
}

/// Run `op` against `repo` synchronously. `Err`'s message carries the full `stderr` (via
/// [`super::git`]) — the only source of why a rebase stopped, so it must never be swallowed
/// (plan M6 step 7).
pub fn execute(repo: &Path, op: &Op, has_upstream: bool) -> Result<String> {
    let owned = op.argv(has_upstream);
    let args: Vec<&str> = owned.iter().map(String::as_str).collect();
    git(repo, &args)
}

/// One op's outcome, sent back over [`spawn`]'s channel.
#[derive(Debug)]
pub struct OpOutcome {
    pub op: Op,
    pub result: std::result::Result<(), String>,
}

/// Run `op` on a background thread so the frame loop stays responsive (plan M6 step 6); the
/// event loop polls the returned channel from its tick, same shape as
/// [`crate::commit_msg::spawn`].
pub fn spawn(repo: PathBuf, op: Op, has_upstream: bool) -> Receiver<OpOutcome> {
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let result = execute(&repo, &op, has_upstream).map(|_| ()).map_err(|e| e.to_string());
        let _ = tx.send(OpOutcome { op, result });
    });
    rx
}

/// The repository's main branch: the one `origin/HEAD` names, else a local `main` or
/// `master`, in that order. `None` when none of them exists.
pub fn main_branch(repo: &Path) -> Option<String> {
    if let Ok(Some(name)) = super::default_branch_name(repo) {
        return Some(name);
    }
    ["main", "master"]
        .into_iter()
        .find(|name| {
            super::git_ok(
                repo,
                &["rev-parse", "--verify", "--quiet", &format!("refs/heads/{name}")],
            )
        })
        .map(str::to_string)
}

/// The branch name a `git branch -a --format=%(HEAD) %(refname:short)` line names, stripping
/// the current-branch `*` marker (mirrors `scm_panel::drawer_ref_name`'s `Branches` arm, which
/// is private to that module and only used from the SCM context menu).
pub fn branch_name(line: &str) -> &str {
    line.trim_start().trim_start_matches('*').split_whitespace().next().unwrap_or("")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn update_pulls_a_merge_never_a_rebase() {
        // `--rebase=false`, not a bare `pull`: the flag is what makes the op independent of the
        // repo's `pull.rebase` setting, so `Update` and `Rebase onto` stay two different acts.
        assert_eq!(Op::Update.argv(true), vec!["pull", "--rebase=false", "--autostash", "--prune"]);
        assert_eq!(
            Op::Update.argv(false),
            vec!["pull", "--rebase=false", "--autostash", "--prune"]
        );
        assert!(Op::Update.confirm_message("main").contains("--rebase=false"));
    }

    #[test]
    fn update_main_off_branch_fast_forwards_without_checkout() {
        let op = Op::UpdateMain { branch: "main".to_string(), checked_out: false };
        assert_eq!(
            op.argv(true),
            vec!["fetch", "origin", "--prune", "refs/heads/main:refs/heads/main"]
        );
        assert!(op.confirm_message("feature").contains("`main`"));
    }

    #[test]
    fn update_main_on_main_is_a_plain_update() {
        let op = Op::UpdateMain { branch: "main".to_string(), checked_out: true };
        assert_eq!(op.argv(true), Op::Update.argv(true));
    }

    #[test]
    fn push_reuses_the_existing_upstream() {
        assert_eq!(Op::Push.argv(true), vec!["push"]);
    }

    #[test]
    fn push_sets_upstream_to_origin_head_when_absent() {
        assert_eq!(Op::Push.argv(false), vec!["push", "-u", "origin", "HEAD"]);
    }

    #[test]
    fn fetch_all_prunes() {
        assert_eq!(Op::FetchAll.argv(true), vec!["fetch", "--all", "--prune"]);
    }

    #[test]
    fn merge_into_names_the_picked_branch_unedited() {
        let op = Op::MergeInto("feature/x".to_string());
        assert_eq!(op.argv(true), vec!["merge", "--no-edit", "feature/x"]);
    }

    #[test]
    fn merge_into_keeps_a_slashed_branch_name_as_one_argument() {
        // No shell is involved (argv, not a command line) — the slash needs no escaping,
        // it just must survive as a single element.
        let op = Op::MergeInto("release/2026-09".to_string());
        assert_eq!(op.argv(true), vec!["merge", "--no-edit", "release/2026-09"]);
    }

    #[test]
    fn rebase_onto_names_the_picked_branch() {
        let op = Op::RebaseOnto("origin/main".to_string());
        assert_eq!(op.argv(true), vec!["rebase", "origin/main"]);
    }

    #[test]
    fn confirm_message_shows_merge_direction() {
        let op = Op::MergeInto("feature/x".to_string());
        assert_eq!(op.confirm_message("main"), "merge `feature/x` into `main`");
    }

    #[test]
    fn confirm_message_shows_rebase_direction() {
        let op = Op::RebaseOnto("main".to_string());
        assert_eq!(op.confirm_message("feature/x"), "rebase `feature/x` onto `main`");
    }

    #[test]
    fn branch_name_strips_the_current_marker_and_whitespace() {
        assert_eq!(branch_name("* main"), "main");
        assert_eq!(branch_name("  feature/x"), "feature/x");
        assert_eq!(branch_name("  origin/release/1.2"), "origin/release/1.2");
    }
}
