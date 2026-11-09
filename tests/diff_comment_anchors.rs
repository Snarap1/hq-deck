//! Diff comments on the new side of a worktree scope follow their file: the agent's edit or a
//! save moves their lines with the text, a held view keeps its cards on the rows it painted, and
//! a comment whose lines were deleted stays, orphaned, with its snippet. An old-side comment
//! counts in the diff's base, which an edit to the worktree does not move, so it stays put.

mod common;

use std::cell::RefCell;

use anyhow::{Result, bail};
use common::{Repo, app_on, land_world, typed};
use hq_deck::app::{App, Focus, Mode};
use hq_deck::diff::Row;
use hq_deck::export::ExportTarget;
use hq_deck::model::Side;

struct FakeTarget {
    ok: bool,
    captured: RefCell<Vec<String>>,
}

impl FakeTarget {
    fn new(ok: bool) -> Self {
        Self { ok, captured: RefCell::new(Vec::new()) }
    }
    fn last(&self) -> String {
        self.captured.borrow().last().cloned().unwrap_or_default()
    }
}

impl ExportTarget for FakeTarget {
    fn label(&self) -> &'static str {
        "fake"
    }
    fn success_message(&self, _: usize) -> String {
        "exported".to_string()
    }
    fn failure_message(&self) -> String {
        "failed".to_string()
    }
    fn export(&self, text: &str) -> Result<()> {
        self.captured.borrow_mut().push(text.to_string());
        if self.ok { Ok(()) } else { bail!("fake export failure") }
    }
}

/// Twelve lines, `l1`..`l12`, with `l6` optionally rewritten to `L6` and `l12` to `L12`.
fn text(l6: &str, l12: &str) -> String {
    (1..=12)
        .map(|n| match n {
            6 => format!("{l6}\n"),
            12 => format!("{l12}\n"),
            n => format!("l{n}\n"),
        })
        .collect()
}

/// A committed `a.rs` and `b.rs`, both edited in the worktree, with `a.rs`'s diff open: its
/// `l6` is rewritten to `L6`, so the diff holds a `-l6` row and a `+L6` row.
fn on_a_diff() -> (Repo, App) {
    let r = Repo::init();
    r.write("a.rs", &text("l6", "l12"));
    r.write("b.rs", "other\n");
    r.commit_all("init");
    r.write("a.rs", &text("L6", "l12"));
    r.write("b.rs", "other\nmore\n");
    let mut app = app_on(&r);
    open(&mut app, "a.rs");
    (r, app)
}

fn open(app: &mut App, path: &str) {
    let row = app
        .file_rows
        .iter()
        .position(|row| row.file_index().is_some_and(|i| app.entries[i].path == path))
        .expect("the file is listed");
    app.select_file(row).unwrap();
}

/// The visible row whose text is exactly `text`.
fn row_of(app: &App, text: &str) -> usize {
    app.visible.iter().position(|r| r.text() == text).unwrap_or_else(|| panic!("no row {text:?}"))
}

fn comment_on(app: &mut App, text: &str, body: &str) {
    app.focus = Focus::Diff;
    app.diff_cursor = row_of(app, text);
    app.start_comment();
    typed(app, body);
    app.submit_comment();
}

fn anchor(app: &App, i: usize) -> (u32, u32, bool) {
    let c = app.store.get(i).expect("the comment survives");
    (c.start, c.end, c.orphaned)
}

/// The text of the row each card of the open file sits under, in store order.
fn card_texts(app: &App) -> Vec<String> {
    app.card_rows().into_iter().map(|(row, _)| app.visible[row].text()).collect()
}

#[test]
fn a_new_side_diff_comment_follows_the_agents_edit() {
    let (r, mut app) = on_a_diff();
    comment_on(&mut app, "L6", "on the rewrite");
    let c = app.store.get(0).unwrap();
    assert_eq!((c.side, c.diff_anchored), (Side::New, true));
    assert_eq!(anchor(&app, 0), (6, 6, false));
    assert_eq!(card_texts(&app), ["L6"]);

    r.write("a.rs", &format!("// one\n// two\n{}", text("L6", "l12")));
    land_world(&mut app);
    assert_eq!(anchor(&app, 0), (8, 8, false), "two lines above push it down two");
    assert_eq!(card_texts(&app), ["L6"], "the card stays under the line it was made on");
}

#[test]
fn an_old_side_diff_comment_stays_on_the_base_line_it_counts_in() {
    let (r, mut app) = on_a_diff();
    comment_on(&mut app, "l6", "what was here?");
    assert_eq!(app.store.get(0).unwrap().side, Side::Old);
    assert!(app.store.get(0).unwrap().basis.is_none(), "it counts in HEAD, not the worktree");

    r.write("a.rs", &format!("// one\n// two\n{}", text("L6", "l12")));
    land_world(&mut app);
    app.carry_comments();
    assert_eq!(anchor(&app, 0), (6, 6, false), "an edit to the worktree does not move HEAD");
    assert_eq!(card_texts(&app), ["l6"]);
}

#[test]
fn deleting_the_commented_line_orphans_a_diff_comment_and_the_export_says_so() {
    let (r, mut app) = on_a_diff();
    comment_on(&mut app, "L6", "why rewrite?");
    // The agent drops the line outright.
    let gone = text("l6", "l12").replacen("l6\n", "", 1);
    r.write("a.rs", &gone);
    land_world(&mut app);
    assert_eq!(app.store.len(), 1, "a comment is never lost to an edit");
    assert_eq!(anchor(&app, 0), (5, 5, true), "it sits on the line above the gap");
    assert!(!app.is_stale(&app.store.get(0).unwrap().clone()), "the file is still changed");
    assert_eq!(card_texts(&app), ["l5"]);

    let target = FakeTarget::new(true);
    assert!(app.export(&target));
    let out = target.last();
    assert!(out.contains("a.rs:5 (since removed)"), "the location says so:\n{out}");
    assert!(out.contains("+L6"), "the snippet still holds the deleted line:\n{out}");
}

#[test]
fn a_comment_carried_onto_a_folded_line_keeps_its_card_under_the_fold() {
    let (r, mut app) = on_a_diff();
    comment_on(&mut app, "L6", "on the rewrite");
    // The agent reverts the rewrite and edits the last line: `l6` is now far from any change.
    r.write("a.rs", &text("l6", "L12"));
    land_world(&mut app);
    assert_eq!(anchor(&app, 0), (6, 6, false), "a rewritten line carries the comment");
    let cards = app.card_rows();
    assert_eq!(cards.len(), 1, "the comment is not lost from the diff");
    assert!(matches!(app.visible[cards[0].0], Row::Fold { .. }), "it sits under the fold");
}

#[test]
fn a_held_diff_keeps_its_cards_on_the_rows_it_painted_until_it_rebuilds() {
    let (r, mut app) = on_a_diff();
    comment_on(&mut app, "L6", "on the rewrite");
    // A second comment in progress holds the view still (Continuity).
    app.diff_cursor = row_of(&app, "l5");
    app.start_comment();
    assert!(matches!(app.mode, Mode::Composing { .. }));
    r.write("a.rs", &format!("new\n{}", text("L6", "l12")));
    land_world(&mut app);
    app.carry_comments();
    assert_eq!(anchor(&app, 0), (6, 6, false), "the poll leaves a painted file to its view");
    assert_eq!(card_texts(&app), ["L6"]);

    // The draft counts its line in the text the reviewer saw, so it lands on `l5` too.
    typed(&mut app, "on l5");
    app.submit_comment();
    land_world(&mut app);
    assert_eq!(anchor(&app, 0), (7, 7, false));
    assert_eq!(anchor(&app, 1), (6, 6, false));
    assert_eq!(card_texts(&app), ["L6", "l5"]);
}

#[test]
fn a_comment_on_a_file_off_screen_moves_on_the_poll() {
    let (r, mut app) = on_a_diff();
    comment_on(&mut app, "L6", "on the rewrite");
    open(&mut app, "b.rs");
    r.write("a.rs", &format!("new\n{}", text("L6", "l12")));
    app.carry_comments();
    assert_eq!(anchor(&app, 0), (7, 7, false));
}

#[test]
fn an_export_carries_a_held_diff_to_the_disk_without_moving_its_cards() {
    let (r, mut app) = on_a_diff();
    comment_on(&mut app, "L6", "on the rewrite");
    app.open_list();
    assert!(app.mode.is_modal(), "the list holds the view still");
    r.write("a.rs", &format!("new\n{}", text("L6", "l12")));
    land_world(&mut app);

    let failing = FakeTarget::new(false);
    assert!(!app.export(&failing));
    assert!(failing.last().contains("a.rs:7"), "the agent gets today's line numbers");
    assert_eq!(anchor(&app, 0), (6, 6, false), "a failed export moves no card");

    let ok = FakeTarget::new(true);
    assert!(app.export(&ok));
    assert!(ok.last().contains("a.rs:7"));
}

#[test]
fn a_comment_on_a_file_that_left_the_changeset_stays_stale_and_where_it_was() {
    let (r, mut app) = on_a_diff();
    comment_on(&mut app, "L6", "on the rewrite");
    // The agent reverts the whole file: no diff left, so the comment is stale, not moved.
    r.write("a.rs", &text("l6", "l12"));
    land_world(&mut app);
    app.carry_comments();
    assert_eq!(app.store.len(), 1);
    assert!(app.is_stale(&app.store.get(0).unwrap().clone()));
    assert_eq!(anchor(&app, 0), (6, 6, false));
}
