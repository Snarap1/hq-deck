//! In-memory review model: scopes, changed files, and comments.
//!
//! Comments live only for the session and are
//! removed by export or delete — never by a refresh.
//! A content comment's line numbers follow its file as it changes ([`CommentStore::rebase`]).

use std::sync::Arc;
use std::time::Duration;

use similar::{DiffOp, DiffTag, TextDiff};

/// Which set of changes the Changes view shows.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Scope {
    Uncommitted,
    Branch,
    LastTurn,
    /// A picked run of commits, diffed `A^` against `B`.
    Commits,
}

impl Scope {
    pub fn label(self) -> &'static str {
        match self {
            Scope::Uncommitted => "uncommitted",
            Scope::Branch => "branch",
            Scope::LastTurn => "last turn",
            Scope::Commits => "commits",
        }
    }

    /// The scope's name in the specs and in config values (`default_scope`): kebab-case,
    /// unlike the header chip's spaced `label`.
    pub fn name(self) -> &'static str {
        match self {
            Scope::Uncommitted => "uncommitted",
            Scope::Branch => "branch",
            Scope::LastTurn => "last-turn",
            Scope::Commits => "commits",
        }
    }

    /// Cycle to the next scope, for the header chip click: uncommitted → branch → last turn →
    /// commits.
    #[must_use]
    pub fn cycle(self) -> Self {
        match self {
            Scope::Uncommitted => Scope::Branch,
            Scope::Branch => Scope::LastTurn,
            Scope::LastTurn => Scope::Commits,
            Scope::Commits => Scope::Uncommitted,
        }
    }
}

/// The `commits` scope's pick: a contiguous run from `oldest` to `newest`, both full commit
/// ids, equal for a run of one.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct CommitPick {
    pub oldest: String,
    pub newest: String,
}

impl CommitPick {
    pub fn single(sha: &str) -> Self {
        Self { oldest: sha.to_string(), newest: sha.to_string() }
    }

    pub fn is_single(&self) -> bool {
        self.oldest == self.newest
    }
}

/// Where a comment's diff was read: the worktree, or the picked run it came from. A diff
/// comment renders only while the active scope reads the same diff, both sides: a run and
/// its newest commit alone share a new side but not an old one.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum Rev {
    Worktree,
    Commit(CommitPick),
}

/// How a file changed within a scope.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ChangeKind {
    Added,
    Modified,
    Deleted,
    Renamed,
    Untracked,
}

impl ChangeKind {
    pub fn marker(self) -> char {
        match self {
            ChangeKind::Added => 'A',
            ChangeKind::Modified => 'M',
            ChangeKind::Deleted => 'D',
            ChangeKind::Renamed => 'R',
            ChangeKind::Untracked => '?',
        }
    }
}

/// A row in the Changes list.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct ChangedFile {
    pub path: String,
    pub kind: ChangeKind,
    pub additions: u32,
    pub deletions: u32,
    /// The old path of a renamed file; `None` for every other kind. Its old content lives
    /// at this path, so a rename diffs real content instead of reading as all-insertion.
    pub previous_path: Option<String>,
}

/// Which side of the diff a comment's lines live on.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Side {
    New,
    Old,
}

/// A reviewer comment anchored to a run of diff lines, carrying the snippet.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Comment {
    pub file: String,
    pub side: Side,
    pub start: u32,
    pub end: u32,
    /// Verbatim diff lines the comment anchors to, each keeping its `+`/`-`/space marker.
    pub lines: String,
    pub text: String,
    /// True when anchored to a diff (the `Changes` tab); false for a File-view content comment
    /// (the `All files` tab). Selects how staleness is judged.
    pub diff_anchored: bool,
    /// Where the new side was read.
    pub rev: Rev,
    /// For a content comment, the worktree text `start`/`end` count in: a newer version of the
    /// file is diffed against it to carry them ([`CommentStore::rebase`]). `None` for a diff
    /// comment, whose numbers belong to its diff, and for a content comment made over text
    /// that is not the worktree's (a commit's preview), which stays where it was put.
    pub basis: Option<Basis>,
    /// Every line the comment anchored to was deleted by a later change to the file. It sits on
    /// the line just above where they were, and `lines` still holds what they said.
    pub orphaned: bool,
}

impl Comment {
    /// The `path:start-end` (or `path:line`) location, with ` (removed)` when old-side and
    /// ` (since removed)` once a change deleted the lines it was made on.
    pub fn location(&self) -> String {
        let range = if self.start == self.end {
            format!("{}:{}", self.file, self.start)
        } else {
            format!("{}:{}-{}", self.file, self.start, self.end)
        };
        match self.side {
            Side::Old => format!("{range} (removed)"),
            Side::New if self.orphaned => format!("{range} (since removed)"),
            Side::New => range,
        }
    }
}

/// One version of a file's worktree text, as a content comment's line numbers count in it.
///
/// The comments made on one version share one copy, and a comparison tries the pointer before
/// the bytes, so a poll that finds the file unchanged costs one `memcmp` at most.
#[derive(Clone)]
pub struct Basis(Arc<str>);

impl Basis {
    pub fn new(text: &str) -> Self {
        Self(Arc::from(text))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl PartialEq for Basis {
    fn eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.0, &other.0) || self.0 == other.0
    }
}

impl Eq for Basis {}

impl std::fmt::Debug for Basis {
    // The whole file would drown every `Debug` of a comment.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Basis({} bytes)", self.0.len())
    }
}

/// How long one rebase may spend diffing a version against the next. Past it `similar` settles
/// for a coarser diff, which still carries every comment, only less tightly.
const REBASE_DIFF_BUDGET: Duration = Duration::from_millis(100);

/// The line ops that turn `old` into `new`, numbered as the File view numbers lines.
fn line_ops(old: &str, new: &str) -> Vec<DiffOp> {
    TextDiff::configure().timeout(REBASE_DIFF_BUDGET).diff_lines(old, new).ops().to_vec()
}

/// Carry the 1-based line range `start..=end` across `ops`: the span of the lines it became, or,
/// when every one of them was deleted, the line just above the gap they left (the first line
/// when the gap is at the top), flagged orphaned.
///
/// A replaced line counts as surviving: the comment follows what it was rewritten into.
fn carry(ops: &[DiffOp], start: u32, end: u32) -> (u32, u32, bool) {
    let s = start.max(1) as usize - 1;
    let e = end.max(start).max(1) as usize; // exclusive
    let mut span: Option<(usize, usize)> = None;
    let mut gap: Option<usize> = None;
    let mut widen = |lo: usize, hi: usize| {
        span = Some(span.map_or((lo, hi), |(a, b)| (a.min(lo), b.max(hi))));
    };
    for op in ops {
        let (tag, old, new) = op.as_tag_tuple();
        let (a, b) = (old.start.max(s), old.end.min(e));
        if a >= b {
            continue;
        }
        match tag {
            DiffTag::Equal => widen(new.start + (a - old.start), new.start + (b - old.start)),
            DiffTag::Replace => widen(new.start, new.end),
            DiffTag::Delete => {
                gap.get_or_insert(new.start);
            }
            // An insertion covers no old line, so it never overlaps.
            DiffTag::Insert => {}
        }
    }
    if let Some((lo, hi)) = span {
        return (lo as u32 + 1, hi as u32, false);
    }
    // Every line was deleted: land beside the gap. With no gap either, the range lay past the
    // end of its own basis, which a comment made on that text cannot; the last line beats
    // nowhere.
    let line = gap.unwrap_or_else(|| ops.last().map_or(0, |op| op.new_range().end)).max(1);
    (line as u32, line as u32, true)
}

/// The in-memory comment list for one worktree review session.
#[derive(Default, Debug)]
pub struct CommentStore {
    items: Vec<Comment>,
}

impl CommentStore {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn len(&self) -> usize {
        self.items.len()
    }

    pub fn is_empty(&self) -> bool {
        self.items.is_empty()
    }

    pub fn iter(&self) -> impl Iterator<Item = &Comment> {
        self.items.iter()
    }

    pub fn get(&self, index: usize) -> Option<&Comment> {
        self.items.get(index)
    }

    /// Append a comment; returns its index.
    pub fn add(&mut self, comment: Comment) -> usize {
        self.items.push(comment);
        self.items.len() - 1
    }

    /// Replace the text of the comment at `index`. Returns `false` if out of range.
    pub fn edit(&mut self, index: usize, text: String) -> bool {
        if let Some(c) = self.items.get_mut(index) {
            c.text = text;
            true
        } else {
            false
        }
    }

    /// Remove and return the comment at `index` (delete, or consume one on export).
    pub fn take(&mut self, index: usize) -> Option<Comment> {
        if index < self.items.len() { Some(self.items.remove(index)) } else { None }
    }

    /// Remove and return every comment (consume-all on a successful export).
    pub fn take_all(&mut self) -> Vec<Comment> {
        std::mem::take(&mut self.items)
    }

    /// The files at least one carried content comment anchors to, each once.
    pub fn content_files(&self) -> Vec<String> {
        let mut files: Vec<String> =
            self.items.iter().filter(|c| c.basis.is_some()).map(|c| c.file.clone()).collect();
        files.sort_unstable();
        files.dedup();
        files
    }

    /// Carry every content comment on `file` to `text`, the file's newest version: each moves
    /// by the diff from the version it counts in, then counts in `text`. A comment is never
    /// dropped here: one whose lines were all deleted lands beside the gap, orphaned, and keeps
    /// its snippet. Comments made on one version share one diff.
    pub fn rebase(&mut self, file: &str, text: &Basis) {
        let mut diffs: Vec<(Basis, Vec<DiffOp>)> = Vec::new();
        for c in self.items.iter_mut().filter(|c| c.file == file) {
            let Some(basis) = &c.basis else { continue };
            if basis == text {
                // Share the newest copy, so the next poll's comparison is a pointer.
                c.basis = Some(text.clone());
                continue;
            }
            let at = diffs.iter().position(|(b, _)| b == basis).unwrap_or_else(|| {
                diffs.push((basis.clone(), line_ops(basis.as_str(), text.as_str())));
                diffs.len() - 1
            });
            let (start, end, orphaned) = carry(&diffs[at].1, c.start, c.end);
            c.start = start;
            c.end = end;
            // Orphaning is sticky: lines typed back where the old ones were are new lines.
            c.orphaned |= orphaned;
            c.basis = Some(text.clone());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{Basis, Comment, CommentStore, Rev, Scope, Side};

    fn comment(file: &str, start: u32, end: u32, text: &str) -> Comment {
        Comment {
            file: file.into(),
            side: Side::New,
            start,
            end,
            lines: "+x".into(),
            text: text.into(),
            diff_anchored: true,
            rev: Rev::Worktree,
            basis: None,
            orphaned: false,
        }
    }

    /// A content comment on `start..=end` of `basis`.
    fn content(start: u32, end: u32, basis: &Basis) -> Comment {
        Comment {
            diff_anchored: false,
            basis: Some(basis.clone()),
            ..comment("a.rs", start, end, "c")
        }
    }

    /// The `(start, end, orphaned)` of every comment in `store`.
    fn anchors(store: &CommentStore) -> Vec<(u32, u32, bool)> {
        store.iter().map(|c| (c.start, c.end, c.orphaned)).collect()
    }

    fn numbered(lines: &[&str]) -> String {
        lines.iter().flat_map(|l| [*l, "\n"]).collect()
    }

    #[test]
    fn a_content_comment_shifts_with_lines_added_and_removed_above_it() {
        let v1 = Basis::new(&numbered(&["a", "b", "c", "d", "e"]));
        let mut s = CommentStore::new();
        s.add(content(3, 4, &v1)); // c..d
        s.add(content(1, 1, &v1)); // a
        let v2 = Basis::new(&numbered(&["new", "new", "a", "b", "c", "d", "e"]));
        s.rebase("a.rs", &v2);
        assert_eq!(anchors(&s), [(5, 6, false), (3, 3, false)]);
        let v3 = Basis::new(&numbered(&["new", "c", "d", "e"]));
        s.rebase("a.rs", &v3);
        assert_eq!(anchors(&s)[0], (2, 3, false), "removals above pull it back up");
        assert_eq!(s.get(0).unwrap().basis.as_ref(), Some(&v3), "it now counts in the new text");
    }

    #[test]
    fn a_rewritten_line_carries_its_comment_and_an_insertion_inside_widens_the_range() {
        let v1 = Basis::new(&numbered(&["a", "b", "c", "d"]));
        let mut s = CommentStore::new();
        s.add(content(2, 2, &v1)); // b, rewritten
        s.add(content(3, 4, &v1)); // c..d, split by an insertion
        let v2 = Basis::new(&numbered(&["a", "B!", "c", "inserted", "d"]));
        s.rebase("a.rs", &v2);
        assert_eq!(anchors(&s), [(2, 2, false), (3, 5, false)]);
    }

    #[test]
    fn a_comment_whose_lines_were_all_deleted_stays_beside_the_gap_orphaned() {
        let v1 = Basis::new(&numbered(&["a", "b", "c", "d", "e"]));
        let mut s = CommentStore::new();
        s.add(content(3, 4, &v1)); // c..d, both deleted
        s.add(content(1, 1, &v1)); // a, deleted at the top
        s.add(content(4, 5, &v1)); // d..e, only d deleted
        let v2 = Basis::new(&numbered(&["b", "e"]));
        s.rebase("a.rs", &v2);
        assert_eq!(s.len(), 3, "nothing is dropped");
        assert_eq!(anchors(&s), [(1, 1, true), (1, 1, true), (2, 2, false)]);
        let gone = s.get(0).unwrap();
        assert_eq!(gone.lines, "+x", "the snippet still says what the lines were");
        assert_eq!(gone.location(), "a.rs:1 (since removed)");
        // Typing lines back is not undoing the removal.
        s.rebase("a.rs", &v1);
        assert!(s.get(0).unwrap().orphaned, "orphaning is sticky");
    }

    #[test]
    fn a_gap_in_the_middle_puts_the_orphan_on_the_line_above_it() {
        let v1 = Basis::new(&numbered(&["a", "b", "c", "d"]));
        let mut s = CommentStore::new();
        s.add(content(2, 3, &v1));
        s.rebase("a.rs", &Basis::new(&numbered(&["a", "d"])));
        assert_eq!(anchors(&s), [(1, 1, true)]);
        s.rebase("a.rs", &Basis::new(""));
        assert_eq!(anchors(&s), [(1, 1, true)], "an emptied file still holds it, at the top");
    }

    #[test]
    fn rebase_leaves_diff_comments_other_files_and_an_unchanged_text_alone() {
        let v1 = Basis::new(&numbered(&["a", "b"]));
        let mut s = CommentStore::new();
        s.add(comment("a.rs", 2, 2, "diff comment"));
        s.add(Comment { file: "b.rs".into(), ..content(2, 2, &v1) });
        s.add(content(2, 2, &v1));
        s.rebase("a.rs", &Basis::new(&numbered(&["a", "b"])));
        assert_eq!(anchors(&s), [(2, 2, false); 3]);
        s.rebase("a.rs", &Basis::new(&numbered(&["x", "a", "b"])));
        assert_eq!(anchors(&s), [(2, 2, false), (2, 2, false), (3, 3, false)]);
        assert_eq!(s.content_files(), ["a.rs", "b.rs"]);
    }

    #[test]
    fn comments_on_different_versions_each_move_by_their_own_diff() {
        let v1 = Basis::new(&numbered(&["a", "b", "c"]));
        let v2 = Basis::new(&numbered(&["x", "a", "b", "c"]));
        let mut s = CommentStore::new();
        s.add(content(2, 2, &v1)); // b in v1
        s.add(content(2, 2, &v2)); // a in v2
        s.rebase("a.rs", &Basis::new(&numbered(&["y", "x", "a", "b", "c"])));
        assert_eq!(anchors(&s), [(4, 4, false), (3, 3, false)]);
    }

    #[test]
    fn line_endings_and_a_missing_final_newline_count_lines_like_the_file_view() {
        let v1 = Basis::new("a\r\nb\r\nc");
        let mut s = CommentStore::new();
        s.add(content(3, 3, &v1));
        s.rebase("a.rs", &Basis::new("top\r\na\r\nb\r\nc"));
        assert_eq!(anchors(&s), [(4, 4, false)]);
    }

    #[test]
    fn scope_cycles_and_labels() {
        // The chip click cycles through all four scopes and wraps.
        assert_eq!(Scope::Uncommitted.cycle(), Scope::Branch);
        assert_eq!(Scope::Branch.cycle(), Scope::LastTurn);
        assert_eq!(Scope::LastTurn.cycle(), Scope::Commits);
        assert_eq!(Scope::Commits.cycle(), Scope::Uncommitted);
        assert_eq!(Scope::Uncommitted.label(), "uncommitted");
        assert_eq!(Scope::LastTurn.label(), "last turn");
        assert_eq!(Scope::Commits.label(), "commits");
        assert_eq!(Scope::Commits.name(), "commits");
    }

    #[test]
    fn a_pick_of_one_is_single() {
        let pick = super::CommitPick::single("abc");
        assert!(pick.is_single());
        assert!(!super::CommitPick { oldest: "a".into(), newest: "b".into() }.is_single());
    }

    #[test]
    fn location_formats_range_single_and_removed() {
        let mut c = comment("a.rs", 40, 52, "x");
        assert_eq!(c.location(), "a.rs:40-52");
        c.end = 40;
        assert_eq!(c.location(), "a.rs:40");
        c.side = Side::Old;
        assert_eq!(c.location(), "a.rs:40 (removed)");
    }

    #[test]
    fn add_get_edit() {
        let mut s = CommentStore::new();
        let i = s.add(comment("a.rs", 1, 1, "first"));
        assert_eq!(s.len(), 1);
        assert_eq!(s.get(i).unwrap().text, "first");
        assert!(s.edit(i, "second".into()));
        assert_eq!(s.get(i).unwrap().text, "second");
        assert!(!s.edit(99, "nope".into()));
    }

    #[test]
    fn take_one_and_take_all_consume() {
        let mut s = CommentStore::new();
        s.add(comment("a.rs", 1, 1, "one"));
        s.add(comment("b.rs", 2, 2, "two"));
        let taken = s.take(0).unwrap();
        assert_eq!(taken.text, "one");
        assert_eq!(s.len(), 1);
        let rest = s.take_all();
        assert_eq!(rest.len(), 1);
        assert!(s.is_empty());
        assert!(s.take(0).is_none());
    }
}
