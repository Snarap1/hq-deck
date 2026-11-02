//! The staging layer: `git status --porcelain -z` parsing and the stage / unstage / commit /
//! discard operations.
//!
//! Parsing is pure and unit-tested; the commands run
//! against the repository root, so the repo-relative paths porcelain reports resolve whatever
//! the pane's cwd is.
//!
//! This is the one module in the crate that writes to the index and the worktree, and it does
//! so only under the user's own gesture — the review side ([`super::scope`]) stays read-only.

use std::collections::HashSet;
use std::path::Path;

use anyhow::{Result, bail};

use super::{git, git_ok};

/// One file in the staged or unstaged list.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FileEntry {
    /// Repo-relative path (the new path, for renames), `/`-separated as git reports it.
    pub path: String,
    /// Rename/copy source, when there is one — unstaging a rename must reset both.
    pub orig: Option<String>,
    /// The status letter to display: M, A, D, R, C, U (untracked), or `!` for merge conflicts.
    pub letter: char,
}

/// One `status --porcelain --branch` read: the branch, both file lists, and the upstream
/// distance from the `##` header.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Status {
    pub branch: String,
    pub staged: Vec<FileEntry>,
    pub unstaged: Vec<FileEntry>,
    /// Commits ahead of / behind the upstream, from the porcelain `##` header.
    pub ahead: usize,
    pub behind: usize,
    /// The branch has an upstream at all (the header carries `...remote`).
    pub has_upstream: bool,
}

/// What one [`stage_under`] call did: how many paths it staged, and how many it deliberately
/// left alone because they live at or inside a NESTED repository. The second number is what
/// lets the UI explain a stage that looks like it did nothing.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Staged {
    pub count: usize,
    pub skipped_nested: usize,
}

/// Read the working tree's status: both file lists plus the branch and its upstream distance.
pub fn status(repo: &Path) -> Result<Status> {
    let out = git(
        repo,
        &["status", "--porcelain", "-z", "--branch", "--renames", "--untracked-files=all"],
    )?;
    Ok(parse_status(&out))
}

/// Stage one entry: `add -A` records modifications, additions, and deletions alike.
pub fn stage(repo: &Path, entry: &FileEntry) -> Result<()> {
    let mut args = vec!["add", "-A", "--", entry.path.as_str()];
    if let Some(original) = entry.orig.as_deref() {
        args.push(original);
    }
    git(repo, &args).map(drop)
}

/// Stage every change in the repository.
pub fn stage_all(repo: &Path) -> Result<()> {
    git(repo, &["add", "-A"]).map(drop)
}

/// Whether HEAD resolves to a real commit — false only on an unborn branch (a repo with no
/// commits yet), where `git reset` has nothing to reset against.
fn has_head(repo: &Path) -> bool {
    git_ok(repo, &["rev-parse", "--verify", "HEAD"])
}

/// Unstage one entry. `reset` needs a HEAD to reset against; on an unborn branch (no commits
/// yet) fall back to dropping the path from the index. The fallback is destructive on a repo
/// WITH a HEAD (`rm --cached` stages a deletion instead of unstaging), so it only runs for the
/// unborn-branch case — any other `reset` failure is propagated instead of swallowed.
pub fn unstage(repo: &Path, entry: &FileEntry) -> Result<()> {
    let mut args = vec!["reset", "-q", "--", entry.path.as_str()];
    if let Some(orig) = entry.orig.as_deref() {
        args.push(orig);
    }
    match git(repo, &args) {
        Ok(_) => Ok(()),
        Err(e) if has_head(repo) => Err(e),
        Err(_) => git(repo, &["rm", "--cached", "-r", "-q", "--", &entry.path]).map(drop),
    }
}

/// Unstage everything, with the same unborn-branch fallback as [`unstage`].
pub fn unstage_all(repo: &Path) -> Result<()> {
    match git(repo, &["reset", "-q"]) {
        Ok(_) => Ok(()),
        Err(e) if has_head(repo) => Err(e),
        Err(_) => git(repo, &["rm", "--cached", "-r", "-q", "--", "."]).map(drop),
    }
}

/// Commit the staged changes; returns git's summary line ("[branch abc1234] …").
pub fn commit(repo: &Path, message: &str) -> Result<String> {
    let out = git(repo, &["commit", "-m", message])?;
    Ok(out.lines().next().unwrap_or("committed").to_string())
}

/// Throw away a file's working-tree changes: untracked files are deleted, tracked ones
/// restored from HEAD. The caller confirms first.
pub fn discard(repo: &Path, entry: &FileEntry) -> Result<()> {
    if entry.letter == 'U' {
        return git(repo, &["clean", "-fd", "--", &entry.path]).map(drop);
    }
    git(repo, &["checkout", "--", &entry.path]).map(drop)
}

/// The diff a commit-message suggestion should describe: the staged diff when something is
/// staged (that is what would be committed), else the working-tree diff. Untracked files only
/// appear as names, so they ride along in the returned path list either way.
pub fn diff_for_message(repo: &Path) -> Result<(String, Vec<String>)> {
    let staged = git(repo, &["diff", "--cached", "--stat", "--patch"])?;
    let (diff, names_args): (String, &[&str]) = if staged.trim().is_empty() {
        let unstaged = git(repo, &["diff", "--stat", "--patch"])?;
        (unstaged, &["diff", "--name-only"])
    } else {
        (staged, &["diff", "--cached", "--name-only"])
    };
    let mut files = nonempty_lines(&git(repo, names_args)?);
    if files.is_empty() {
        // Nothing tracked changed: describe the untracked files instead.
        files = nonempty_lines(&git(repo, &["ls-files", "--others", "--exclude-standard"])?);
    }
    Ok((diff, files))
}

/// Repo-relative roots of ignored paths. Deliberately a SECOND status call with
/// `--untracked-files=normal`: `--ignored` combined with the `-uall` [`status`] uses expands
/// every file inside `target/` and `node_modules/`, while `normal` keeps ignored directories
/// collapsed to a single `dir/` entry — one cheap line instead of tens of thousands.
pub fn ignored(repo: &Path) -> Result<Vec<String>> {
    let out = git(
        repo,
        &["status", "--porcelain", "-z", "--ignored=traditional", "--untracked-files=normal"],
    )?;
    Ok(parse_ignored(&out))
}

/// Stage everything under `target` that belongs to THIS repository — additions, modifications
/// and deletions alike.
///
/// Rather than handing git the directory (`git add -A -- dir`), the paths come from this
/// repo's own [`status`], filtered to the target subtree and then stripped of anything at or
/// inside a NESTED repository root. A bare `git add` on a directory holding an unregistered
/// inner repo records it as a gitlink ("adding embedded git repository"); enumerating instead
/// makes the boundary explicit, and a nested repo's changes can only ever be staged by
/// selecting something inside it.
pub fn stage_under(repo: &Path, target: &Path) -> Result<Staged> {
    let prefix = rel_of(repo, target)?;
    let status = status(repo)?;
    let candidates = paths_under(&status, prefix.as_deref());
    let before = candidates.len();
    let nested = nested_roots_for(repo, &candidates);
    let paths = drop_nested(candidates, &nested);
    let skipped_nested = before - paths.len();
    if paths.is_empty() {
        return Ok(Staged { count: 0, skipped_nested });
    }
    // A command line is capped at a couple of hundred kilobytes: stage in batches so a huge
    // untracked tree does not blow past it.
    for chunk in paths.chunks(64) {
        let mut args = vec!["add", "-A", "--"];
        args.extend(chunk.iter().map(String::as_str));
        git(repo, &args)?;
    }
    Ok(Staged { count: paths.len(), skipped_nested })
}

/// `target` as a `/`-separated repo-relative path; `None` when it IS the repo root (no prefix
/// = the whole repo). Refuse an outside or differently-normalized path rather than
/// accidentally staging the whole repository.
fn rel_of(repo: &Path, target: &Path) -> Result<Option<String>> {
    let Ok(rel) = target.strip_prefix(repo) else {
        bail!("{} is outside repository {}", target.display(), repo.display());
    };
    let joined = rel
        .components()
        .map(|c| c.as_os_str().to_string_lossy().into_owned())
        .collect::<Vec<_>>()
        .join("/");
    Ok((!joined.is_empty()).then_some(joined))
}

/// The nested-repository roots that lie on the way to any of `paths`: every ancestor prefix
/// (and the path itself) that carries its own `.git`, excluding this repo's root.
fn nested_roots_for(repo: &Path, paths: &[String]) -> Vec<String> {
    let mut seen: HashSet<String> = HashSet::new();
    let mut roots: Vec<String> = Vec::new();
    for path in paths {
        let parts: Vec<&str> = path.trim_end_matches('/').split('/').collect();
        for end in 1..=parts.len() {
            let prefix = parts[..end].join("/");
            if !seen.insert(prefix.clone()) {
                continue;
            }
            let mut abs = repo.to_path_buf();
            for part in &parts[..end] {
                abs.push(part);
            }
            if abs.join(".git").exists() {
                roots.push(prefix);
            }
        }
    }
    roots
}

/// Parse `git status --porcelain -z --branch` output. Entries are NUL-separated `XY path`; a
/// rename/copy is followed by a second NUL-separated field holding the source path. X is the
/// index (staged) state, Y the worktree state.
#[must_use]
pub fn parse_status(raw: &str) -> Status {
    let mut status = Status::default();
    let mut parts = raw.split('\0');
    while let Some(entry) = parts.next() {
        if entry.is_empty() {
            continue;
        }
        if let Some(header) = entry.strip_prefix("## ") {
            status.branch = parse_branch(header);
            (status.ahead, status.behind) = parse_ahead_behind(header);
            status.has_upstream = header.contains("...");
            continue;
        }
        let Some(((x, y), path)) = split_entry(entry) else {
            continue;
        };
        let orig = if matches!(x, 'R' | 'C') || matches!(y, 'R' | 'C') {
            parts.next().filter(|s| !s.is_empty()).map(str::to_string)
        } else {
            None
        };
        let path = path.to_string();
        if x == '?' && y == '?' {
            status.unstaged.push(FileEntry { path, orig: None, letter: 'U' });
            continue;
        }
        if x == '!' {
            continue; // ignored file
        }
        if is_conflict(x, y) {
            status.unstaged.push(FileEntry { path, orig, letter: '!' });
            continue;
        }
        if x != ' ' {
            status.staged.push(FileEntry {
                path: path.clone(),
                orig: orig.clone(),
                letter: display_letter(x),
            });
        }
        if y != ' ' {
            status.unstaged.push(FileEntry { path, orig, letter: display_letter(y) });
        }
    }
    status
}

/// The repo-relative paths a stage under `prefix` should touch: the WORKING-TREE side of
/// `status` (already-staged entries need no re-adding), deduped and ordered. `None` = the
/// whole repository.
#[must_use]
pub fn paths_under(status: &Status, prefix: Option<&str>) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for entry in &status.unstaged {
        let rename_touches_prefix =
            entry.orig.as_deref().is_some_and(|original| under(original, prefix));
        if !under(&entry.path, prefix) && !rename_touches_prefix {
            continue;
        }
        for path in std::iter::once(&entry.path).chain(entry.orig.iter()) {
            if !out.contains(path) {
                out.push(path.clone());
            }
        }
    }
    out.sort();
    out
}

/// Drop every path that sits AT or inside one of `nested` — the nested repository roots that
/// must not be staged from an outer repo.
#[must_use]
pub fn drop_nested(paths: Vec<String>, nested: &[String]) -> Vec<String> {
    paths.into_iter().filter(|path| !nested.iter().any(|root| under(path, Some(root)))).collect()
}

/// Path-prefix containment on `/`-separated repo-relative paths: equal, or a real descendant.
/// `None` contains everything.
#[must_use]
pub fn under(path: &str, prefix: Option<&str>) -> bool {
    let Some(prefix) = prefix else { return true };
    let prefix = prefix.trim_end_matches('/');
    let path = path.trim_end_matches('/');
    path == prefix
        || (path.len() > prefix.len()
            && path.starts_with(prefix)
            && path.as_bytes()[prefix.len()] == b'/')
}

/// The `!!` entries of a `--ignored` porcelain run: repo-relative roots of ignored files and
/// (collapsed) ignored directories, trailing `/` stripped. Rename source fields can never
/// start with `!! `, so a plain scan is safe without tracking the two-field rename shape.
#[must_use]
pub fn parse_ignored(raw: &str) -> Vec<String> {
    raw.split('\0')
        .filter_map(|entry| entry.strip_prefix("!! "))
        .map(|path| path.trim_end_matches('/').to_string())
        .filter(|path| !path.is_empty())
        .collect()
}

/// `("XY", path)` from one porcelain entry; the XY columns are always ASCII.
fn split_entry(entry: &str) -> Option<((char, char), &str)> {
    let bytes = entry.as_bytes();
    if bytes.len() < 4 || bytes[2] != b' ' {
        return None;
    }
    Some(((bytes[0] as char, bytes[1] as char), &entry[3..]))
}

/// The seven unmerged porcelain pairs: DD, AU, UD, UA, DU, AA, UU.
fn is_conflict(x: char, y: char) -> bool {
    matches!((x, y), ('D' | 'U', 'D') | ('A' | 'D' | 'U', 'U') | ('U' | 'A', 'A'))
}

/// Type changes (T) read as plain modifications.
fn display_letter(c: char) -> char {
    if c == 'T' { 'M' } else { c }
}

/// Branch from the `## …` header: `main...origin/main [ahead 1]`, bare `main`,
/// `No commits yet on main`, or `HEAD (no branch)` when detached.
fn parse_branch(header: &str) -> String {
    let head = header.split("...").next().unwrap_or(header);
    head.strip_prefix("No commits yet on ").unwrap_or(head).to_string()
}

/// `(ahead, behind)` from the header's `[ahead 1, behind 2]` suffix (either half may be
/// absent; `[gone]` and no-bracket headers give zeros).
fn parse_ahead_behind(header: &str) -> (usize, usize) {
    let Some(bracket) = header.rsplit_once('[').map(|(_, b)| b.trim_end_matches(']')) else {
        return (0, 0);
    };
    let count_after = |tag: &str| {
        bracket
            .split(',')
            .map(str::trim)
            .find_map(|part| part.strip_prefix(tag))
            .and_then(|n| n.trim().parse().ok())
            .unwrap_or(0)
    };
    (count_after("ahead "), count_after("behind "))
}

/// Non-empty lines of one git read, as owned strings.
fn nonempty_lines(out: &str) -> Vec<String> {
    out.lines().filter(|l| !l.is_empty()).map(str::to_string).collect()
}

#[cfg(test)]
mod tests {
    use super::{FileEntry, Status, drop_nested, parse_ignored, parse_status, paths_under, under};

    fn entry(path: &str, letter: char, orig: Option<&str>) -> FileEntry {
        FileEntry { path: path.to_string(), orig: orig.map(str::to_string), letter }
    }

    #[test]
    fn parse_status_reads_the_branch_header_in_every_shape() {
        assert_eq!(parse_status("## main...origin/main [ahead 1]\0").branch, "main");
        assert_eq!(parse_status("## git-panel\0").branch, "git-panel");
        assert_eq!(parse_status("## No commits yet on trunk\0").branch, "trunk");
        assert_eq!(parse_status("## HEAD (no branch)\0").branch, "HEAD (no branch)");
    }

    #[test]
    fn parse_status_reads_ahead_behind_and_upstream_presence() {
        let s = parse_status("## main...origin/main [ahead 3, behind 2]\0");
        assert_eq!((s.ahead, s.behind, s.has_upstream), (3, 2, true));
        let s = parse_status("## main...origin/main [behind 4]\0");
        assert_eq!((s.ahead, s.behind, s.has_upstream), (0, 4, true));
        // An upstream with no divergence still reports one.
        let s = parse_status("## main...origin/main\0");
        assert_eq!((s.ahead, s.behind, s.has_upstream), (0, 0, true));
        // No upstream at all: the header carries no `...`.
        let s = parse_status("## local-only\0");
        assert_eq!((s.ahead, s.behind, s.has_upstream), (0, 0, false));
        // A deleted upstream reads as no divergence, not as a parse failure.
        let s = parse_status("## main...origin/main [gone]\0");
        assert_eq!((s.ahead, s.behind), (0, 0));
    }

    #[test]
    fn parse_status_reads_a_rename_as_one_entry_with_its_source() {
        // A rename is two NUL-separated fields: the entry, then the source path.
        let s = parse_status("## main\0R  new.rs\0old.rs\0 M other.rs\0");
        assert_eq!(s.staged, [entry("new.rs", 'R', Some("old.rs"))]);
        assert_eq!(s.unstaged, [entry("other.rs", 'M', None)]);

        // A staged rename with a further working-tree edit lands in both lists, and the
        // source travels with both — unstaging a rename must reset the pair.
        let s = parse_status("## main\0RM new.rs\0old.rs\0");
        assert_eq!(s.staged, [entry("new.rs", 'R', Some("old.rs"))]);
        assert_eq!(s.unstaged, [entry("new.rs", 'M', Some("old.rs"))]);
    }

    #[test]
    fn parse_status_splits_the_two_columns_and_marks_conflicts() {
        let s = parse_status(
            "## main\0M  staged.rs\0 M dirty.rs\0MM both.rs\0?? new.rs\0!! build/out\0",
        );
        assert_eq!(s.staged, [entry("staged.rs", 'M', None), entry("both.rs", 'M', None)]);
        assert_eq!(
            s.unstaged,
            [entry("dirty.rs", 'M', None), entry("both.rs", 'M', None), entry("new.rs", 'U', None)]
        );

        // A conflict is one unstaged entry marked `!`, never a staged/unstaged pair.
        let s = parse_status("## main\0UU clash.rs\0AA added.rs\0DU gone.rs\0");
        assert_eq!(
            s.unstaged,
            [
                entry("clash.rs", '!', None),
                entry("added.rs", '!', None),
                entry("gone.rs", '!', None)
            ]
        );
        assert!(s.staged.is_empty(), "a conflict is never staged: {:?}", s.staged);

        // A type change displays as a plain modification.
        assert_eq!(parse_status("## main\0T  link.rs\0").staged, [entry("link.rs", 'M', None)]);
    }

    #[test]
    fn under_matches_equality_and_real_descendants_only() {
        assert!(under("src/app.rs", None), "no prefix contains everything");
        assert!(under("src", Some("src")), "a prefix contains itself");
        assert!(under("src/app.rs", Some("src")));
        assert!(under("src/git/mod.rs", Some("src/git")));
        // A shared textual prefix is not containment.
        assert!(!under("srcery/app.rs", Some("src")));
        assert!(!under("src", Some("src/git")), "a parent is not under its child");
        // Trailing slashes on either side do not change the answer.
        assert!(under("src/app.rs", Some("src/")));
        assert!(under("src/", Some("src")));
    }

    #[test]
    fn drop_nested_removes_paths_at_or_inside_a_nested_repository() {
        let paths = vec![
            "README.md".to_string(),
            "vendor".to_string(),
            "vendor/lib/x.rs".to_string(),
            "vendored/keep.rs".to_string(),
        ];
        assert_eq!(
            drop_nested(paths, &["vendor".to_string()]),
            ["README.md".to_string(), "vendored/keep.rs".to_string()]
        );
    }

    #[test]
    fn paths_under_takes_the_working_tree_side_and_follows_a_renames_source() {
        let status = Status {
            unstaged: vec![
                entry("src/app.rs", 'M', None),
                entry("docs/plan.md", 'M', None),
                // The rename landed outside the prefix, but its source is inside it: staging
                // the prefix has to record both halves or the rename splits in two.
                entry("moved/new.rs", 'R', Some("src/old.rs")),
            ],
            staged: vec![entry("already.rs", 'A', None)],
            ..Status::default()
        };
        assert_eq!(
            paths_under(&status, Some("src")),
            ["moved/new.rs".to_string(), "src/app.rs".to_string(), "src/old.rs".to_string()]
        );
        // No prefix: everything on the working-tree side, staged entries never re-added.
        assert_eq!(
            paths_under(&status, None),
            [
                "docs/plan.md".to_string(),
                "moved/new.rs".to_string(),
                "src/app.rs".to_string(),
                "src/old.rs".to_string(),
            ]
        );
    }

    #[test]
    fn parse_ignored_reads_the_bang_entries_and_strips_trailing_slashes() {
        let raw = "## main\0?? new.rs\0!! target/\0!! .env\0!! node_modules/\0";
        assert_eq!(parse_ignored(raw), ["target", ".env", "node_modules"]);
        assert!(parse_ignored("## main\0?? new.rs\0").is_empty());
    }
}
