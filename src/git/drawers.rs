//! The SCM drawers: display-only lists behind the source-control panel — graph, commits,
//! branches, remotes, stashes, tags, worktrees.
//!
//! Every read here is a plain `git` list rendered as
//! lines; nothing in this module writes.

use std::path::Path;

use anyhow::Result;

use super::git;

/// `git log --graph --oneline`, newest first.
pub fn graph(repo: &Path, limit: usize) -> Result<Vec<String>> {
    let n = format!("-{limit}");
    Ok(lines(&git(repo, &["log", "--graph", "--oneline", "--decorate=short", &n])?))
}

/// `git log --oneline`, newest first.
pub fn commits(repo: &Path, limit: usize) -> Result<Vec<String>> {
    let n = format!("-{limit}");
    Ok(lines(&git(repo, &["log", "--oneline", "--decorate=short", "--date=short", &n])?))
}

/// One file's history, following renames.
pub fn file_history(repo: &Path, path: &str, limit: usize) -> Result<Vec<String>> {
    let n = format!("-{limit}");
    Ok(lines(&git(repo, &["log", "--oneline", "--follow", &n, "--", path])?))
}

/// Local + remote branches, most recently committed first; the current one carries git's `*`.
pub fn branches(repo: &Path) -> Result<Vec<String>> {
    Ok(lines(&git(
        repo,
        &["branch", "-a", "--sort=-committerdate", "--format=%(HEAD) %(refname:short)"],
    )?))
}

/// One line per remote. `remote -v` lists fetch and push separately, so only the fetch rows
/// are kept.
pub fn remotes(repo: &Path) -> Result<Vec<String>> {
    let out = git(repo, &["remote", "-v"])?;
    Ok(out
        .lines()
        .filter_map(|line| line.strip_suffix(" (fetch)"))
        .map(|rest| rest.replace('\t', "  "))
        .collect())
}

/// `git stash list`.
pub fn stashes(repo: &Path) -> Result<Vec<String>> {
    Ok(lines(&git(repo, &["stash", "list"])?))
}

/// Tags, newest first.
pub fn tags(repo: &Path) -> Result<Vec<String>> {
    Ok(lines(&git(repo, &["tag", "--sort=-creatordate"])?))
}

/// One line per worktree (`git worktree list`): path, short head, [branch] — the primary
/// checkout first.
pub fn worktrees(repo: &Path) -> Result<Vec<String>> {
    Ok(lines(&git(repo, &["worktree", "list"])?))
}

fn lines(out: &str) -> Vec<String> {
    out.lines().filter(|l| !l.is_empty()).map(str::to_string).collect()
}
