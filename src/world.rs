//! The world snapshot: the derived state one refresh produces, built from git alone.
//!
//! `build` reads nothing from `App`, so the same call runs synchronously (startup, scope
//! switches, first visits) and behind the worker (polls, `r`, return visits)
//! Reconciling a snapshot into place state stays
//! in `App::reconcile_world`, the one home for the Continuity rules.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::mpsc::{Receiver, Sender};

use anyhow::Result;

use crate::app::Tab;
use crate::file_list::{Annotation, Entry};
use crate::git;
use crate::model::{ChangedFile, CommitPick, Scope};

/// Everything the build reads. A landed snapshot reconciles only while the view still
/// matches the input that produced it.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct WorldInput {
    pub repo: PathBuf,
    pub tab: Tab,
    pub scope: Scope,
    /// The `--base` flag, resolved fresh per build. The pick is read from its ref at build
    /// time, so it is derived output, never input identity — a pick made in another pane
    /// of this worktree must land here as newer content, not be discarded as a mismatch.
    pub base: Option<String>,
    /// Bumped by this pane's own pick, so a build that read the previous pick fails the
    /// landing's input-equality gate instead of reverting the picked base. Another pane's
    /// pick leaves it alone — see `base` above.
    pub base_epoch: u64,
    /// The `last-turn` baseline tree the changed set diffs against; `None` before a turn.
    pub turn_baseline: Option<String>,
    /// The `commits` scope's pick. Part of the identity, so a build for a replaced pick
    /// fails the landing gate instead of painting the old run.
    pub commit_pick: Option<CommitPick>,
    /// Expanded ignored directories whose children the `All files` tree loads.
    pub toggled_dirs: HashSet<String>,
}

/// The derived state one refresh produces: the scope changeset, the navigator entries, and
/// the `branch` scope's resolved base. The base rides the snapshot so the header name and
/// the changeset it heads land whole, from one build.
#[derive(Debug)]
pub struct WorldSnapshot {
    pub changed: HashMap<String, Annotation>,
    pub entries: Vec<Entry>,
    pub branch_base: git::BaseStatus,
    /// The `commits` scope's pick verdict, from the same build as the changeset it heads
    /// `None` on every other scope.
    pub pick_status: Option<PickStatus>,
    /// The commit `HEAD` named when the build ran, the commit picker's universe key
    /// `None` in an unborn repository.
    pub head: Option<String>,
}

/// What one build found the commit pick to be.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum PickVerdict {
    /// Every commit reachable from `HEAD`.
    Live,
    /// Some commit unreachable from `HEAD`. The run still paints.
    OffBranch,
    /// A needed commit is pruned, named here. The scope is empty.
    Gone(String),
}

/// The pick's verdict and the newest commit's subject, for the header.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct PickStatus {
    pub verdict: PickVerdict,
    pub subject: String,
    /// How many commits the run spans, `0` when `gone` or not a run.
    pub count: usize,
}

/// The scope-dependent half of a build: the changeset and the base or pick it diffs against,
/// landed together so the header and the list never disagree.
#[derive(Debug)]
pub struct ScopeBuild {
    pub branch_base: git::BaseStatus,
    pub pick_status: Option<PickStatus>,
    pub changed: Vec<ChangedFile>,
}

/// Build the snapshot for `input`. The changeset is computed regardless of tab so the
/// header count and comment staleness stay correct while `All files` lists the whole
/// worktree. In `last-turn` with no baseline yet, the changeset is empty until a mark
/// start is observed.
pub fn build(input: &WorldInput) -> Result<WorldSnapshot> {
    // Outside a git repo, an empty snapshot paints the quiet empty state rather than a
    // failing status line every poll.
    if !git::is_repo(&input.repo) {
        return Ok(WorldSnapshot {
            changed: HashMap::new(),
            entries: Vec::new(),
            branch_base: git::BaseStatus::default(),
            pick_status: None,
            head: None,
        });
    }
    let ScopeBuild { branch_base, pick_status, changed } = build_changed(input)?;
    let head = git::head_oid(&input.repo);
    let changed_map = annotate(&changed);
    let entries = match input.tab {
        // The whole worktree (ignored included), with expanded ignored dirs loaded lazily.
        Tab::AllFiles => all_files_entries(input, &changed_map)?,
        // `Changes` (the `PR` tab never builds a snapshot).
        _ => changed.iter().map(Entry::from_changed).collect(),
    };
    Ok(WorldSnapshot { changed: changed_map, entries, branch_base, pick_status, head })
}

/// The active scope's changed files and, on the `branch` scope, the base they diff against —
/// the piece a scope switch rebuilds before its frame, so the header count and list never
/// wear another scope's label.
pub fn build_changed(input: &WorldInput) -> Result<ScopeBuild> {
    let plain = |changed| ScopeBuild {
        branch_base: git::BaseStatus::default(),
        pick_status: None,
        changed,
    };
    if !git::is_repo(&input.repo) {
        return Ok(plain(Vec::new()));
    }
    match input.scope {
        Scope::LastTurn => match input.turn_baseline.as_deref() {
            Some(t) => Ok(plain(git::changed_against_tree(&input.repo, t)?)),
            None => Ok(plain(Vec::new())),
        },
        Scope::Uncommitted => Ok(plain(git::changed_files(&input.repo, input.scope, None)?)),
        Scope::Branch => {
            // A resolve failure fails the build whole, so the landing keeps the stale
            // frame and reports — degrading to an empty snapshot would blank a populated
            // view over a transient error (Continuity). A chain where
            // nothing resolves is not a failure: it returns the legible no-base state.
            let resolution = git::resolve_base(&input.repo, input.base.as_deref())
                .map_err(|e| anyhow::anyhow!("{}", e.0))?;
            let base_oid = resolution.status.winner.as_ref().map(|w| w.oid().to_string());
            let changed = git::changed_files(&input.repo, input.scope, base_oid.as_deref())?;
            Ok(ScopeBuild { branch_base: resolution.status, pick_status: None, changed })
        }
        Scope::Commits => {
            // The scope is never entered without a pick; a tag without one
            // builds the empty changeset rather than failing the landing.
            let Some(pick) = &input.commit_pick else { return Ok(plain(Vec::new())) };
            let (status, changed) = build_pick(&input.repo, pick)?;
            Ok(ScopeBuild {
                branch_base: git::BaseStatus::default(),
                pick_status: Some(status),
                changed,
            })
        }
    }
}

/// The pick's changeset and verdict in one pass: `gone`
/// once any needed commit, `A^` included, is pruned, else `off branch` once any is
/// unreachable from `HEAD`, else live. A `gone` pick has an empty changeset.
fn build_pick(repo: &Path, pick: &CommitPick) -> Result<(PickStatus, Vec<ChangedFile>)> {
    let gone = |sha: &str| {
        (
            PickStatus {
                verdict: PickVerdict::Gone(sha.to_string()),
                subject: String::new(),
                count: 0,
            },
            Vec::new(),
        )
    };
    if !git::commit_exists(repo, &pick.newest) {
        return Ok(gone(&pick.newest));
    }
    let Some(old) = git::parent_or_empty(repo, &pick.oldest) else {
        return Ok(gone(&pick.oldest));
    };
    if old != git::EMPTY_TREE && !git::commit_exists(repo, &old) {
        return Ok(gone(&old));
    }
    let subject = git::commit_subject(repo, &pick.newest).unwrap_or_default();
    let count = git::run_length_from(repo, &old, &pick.oldest, &pick.newest).unwrap_or(0);
    let changed = git::changed_between(repo, &old, &pick.newest)?;
    // The oldest is an ancestor of the newest, so one reachability check covers the run.
    let verdict = if git::is_reachable(repo, &pick.newest) {
        PickVerdict::Live
    } else {
        PickVerdict::OffBranch
    };
    Ok((PickStatus { verdict, subject, count }, changed))
}

/// The changed-files map every consumer keys by path — one construction site, shared by
/// the worker build and the scope switch's synchronous rebuild.
pub fn annotate(changed: &[ChangedFile]) -> HashMap<String, Annotation> {
    changed.iter().map(|f| (f.path.clone(), Annotation::from(f))).collect()
}

/// The persisted turn baseline for `repo`, if any — the one seeding rule, shared by the
/// worker's tracker and the app's first-frame mirror.
pub fn seed_baseline(repo: &std::path::Path) -> Option<String> {
    git::read_baseline_ref(repo)
}

/// The `All files` entries: every worktree path (ignored dimmed), with the children of
/// expanded ignored directories loaded lazily. Only directories the
/// user has expanded are walked, so the cost tracks what is on screen, not the whole tree.
pub(crate) fn all_files_entries(
    input: &WorldInput,
    changed: &HashMap<String, Annotation>,
) -> Result<Vec<Entry>> {
    let to_entry = |w: git::WorktreeEntry| Entry {
        annotation: changed.get(&w.path).cloned(),
        path: w.path,
        previous_path: None,
        ignored: w.ignored,
        is_dir: w.is_dir,
    };
    let mut entries: Vec<Entry> = git::all_files(&input.repo)?.into_iter().map(&to_entry).collect();
    let mut i = 0;
    while i < entries.len() {
        if entries[i].is_dir && input.toggled_dirs.contains(&entries[i].path) {
            let path = entries[i].path.clone();
            let children = git::list_ignored_dir(&input.repo, &path).into_iter().map(&to_entry);
            entries.extend(children);
        }
        i += 1;
    }
    Ok(entries)
}

/// One queued refresh's attributes, accumulated on `App` until the loop dispatches it.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct WorldRequest {
    /// Re-reveal the cursor when the result lands — user-initiated switches only.
    pub reveal: bool,
}

/// One refresh request. The worker builds against `input`, refreshing its `turn_baseline`
/// from the mark ref first, and echoes the tag back with the completion.
#[derive(Debug)]
pub struct WorldJob {
    pub generation: u64,
    pub input: WorldInput,
    /// A user-initiated switch re-reveals the cursor when its result lands; a poll never
    /// does.
    pub reveal: bool,
}

/// A finished job: the tag it was built for and the snapshot — `None` when the
/// input's tab builds no file tree (the `PR` tab).
#[derive(Debug)]
pub struct WorldCompletion {
    pub generation: u64,
    pub input: WorldInput,
    pub reveal: bool,
    pub snapshot: Option<Result<WorldSnapshot>>,
}

/// Run the world worker until the request channel closes. The latest request wins: queued
/// requests coalesce into the newest, keeping any superseded job's reveal flag.
pub fn spawn(rx: Receiver<WorldJob>, tx: Sender<WorldCompletion>) -> std::thread::JoinHandle<()> {
    std::thread::Builder::new()
        .name("world".into())
        .spawn(move || {
            while let Ok(mut job) = rx.recv() {
                while let Ok(next) = rx.try_recv() {
                    job = WorldJob { reveal: job.reveal || next.reveal, ..next };
                }
                // The mark is read from its ref at build time, like the base pick, so a mark
                // set in another hq-deck on this worktree lands here as newer content.
                job.input.turn_baseline = seed_baseline(&job.input.repo);
                let snapshot = job.input.tab.is_file_tab().then(|| build(&job.input));
                let completion = WorldCompletion {
                    generation: job.generation,
                    input: job.input,
                    reveal: job.reveal,
                    snapshot,
                };
                if tx.send(completion).is_err() {
                    break;
                }
            }
        })
        .expect("spawn world worker")
}
