//! Content comments follow their file: an edit by the agent or from the built-in editor moves
//! a File-view comment's lines with the text, a held view keeps its cards on the rows it
//! painted, and a comment whose lines were deleted stays, orphaned, with its snippet.

mod common;

use std::cell::RefCell;

use anyhow::{Result, bail};
use common::{Repo, app_on, enter_tab, land_world, typed};
use hq_deck::app::{App, Focus, Mode, Tab};
use hq_deck::export::ExportTarget;
use hq_deck::handle_key;
use hq_deck::keymap::Keymap;
use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::layout::Rect;

const AREA: Rect = Rect { x: 0, y: 0, width: 120, height: 40 };

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

const FIVE: &str = "alpha\nbeta\ngamma\ndelta\nepsilon\n";

/// A committed `a.rs` of five lines and a `b.rs`, on the `All files` tab with `a.rs` open.
fn on_a() -> (Repo, App) {
    let r = Repo::init();
    r.write("a.rs", FIVE);
    r.write("b.rs", "other\n");
    r.commit_all("init");
    let mut app = app_on(&r);
    enter_tab(&mut app, Tab::AllFiles);
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

/// Comment on File-view rows `first..=last` (0-based) of the open file.
fn comment_rows(app: &mut App, first: usize, last: usize, text: &str) {
    app.focus = Focus::Diff;
    app.diff_cursor = first;
    if last != first {
        app.toggle_select();
        app.diff_cursor = last;
    }
    app.start_comment();
    typed(app, text);
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
fn a_file_view_comment_follows_the_agents_edit_to_the_open_file() {
    let (r, mut app) = on_a();
    comment_rows(&mut app, 2, 2, "on gamma");
    assert_eq!(card_texts(&app), ["gamma"]);

    r.write("a.rs", &format!("// one\n// two\n{FIVE}"));
    land_world(&mut app);
    assert_eq!(anchor(&app, 0), (5, 5, false), "two lines above push it down two");
    assert_eq!(card_texts(&app), ["gamma"], "the card stays under the line it was made on");
}

#[test]
fn a_comment_on_a_file_off_screen_moves_on_the_poll() {
    let (r, mut app) = on_a();
    comment_rows(&mut app, 3, 4, "on delta..epsilon");
    open(&mut app, "b.rs");
    r.write("a.rs", "alpha\ngamma\ndelta\nepsilon\n");
    app.carry_comments();
    assert_eq!(anchor(&app, 0), (3, 4, false));

    // The `PR` tab builds no file tree, and the poll still carries it.
    app.set_tab(Tab::Pr).unwrap();
    r.write("a.rs", "new\nalpha\ngamma\ndelta\nepsilon\n");
    app.carry_comments();
    assert_eq!(anchor(&app, 0), (4, 5, false));
}

#[test]
fn a_held_view_keeps_its_cards_on_the_rows_it_painted_until_it_rebuilds() {
    let (r, mut app) = on_a();
    comment_rows(&mut app, 2, 2, "on gamma");
    // A second comment in progress holds the view still (Continuity).
    app.diff_cursor = 0;
    app.start_comment();
    assert!(matches!(app.mode, Mode::Composing { .. }));
    r.write("a.rs", &format!("new\n{FIVE}"));
    land_world(&mut app);
    app.carry_comments();
    assert_eq!(anchor(&app, 0), (3, 3, false), "the poll leaves a painted file to its view");
    assert_eq!(card_texts(&app), ["gamma"]);

    // The draft counts its line in the text the reviewer saw, so it lands on `alpha` too.
    typed(&mut app, "on alpha");
    app.submit_comment();
    land_world(&mut app);
    assert_eq!(anchor(&app, 0), (4, 4, false));
    assert_eq!(anchor(&app, 1), (2, 2, false));
    assert_eq!(card_texts(&app), ["gamma", "alpha"]);
}

#[test]
fn deleting_the_commented_lines_orphans_the_comment_and_the_export_says_so() {
    let (r, mut app) = on_a();
    comment_rows(&mut app, 1, 2, "why both?");
    r.write("a.rs", "alpha\ndelta\nepsilon\n");
    land_world(&mut app);
    assert_eq!(app.store.len(), 1, "a comment is never lost to an edit");
    assert_eq!(anchor(&app, 0), (1, 1, true), "it sits on the line above the gap");
    assert_eq!(card_texts(&app), ["alpha"]);

    let target = FakeTarget::new(true);
    assert!(app.export(&target));
    let out = target.last();
    assert!(out.starts_with("a.rs:1 (since removed)\n"), "the location says so:\n{out}");
    assert!(out.contains(" beta\n gamma"), "the snippet still holds the deleted lines:\n{out}");
}

#[test]
fn an_export_carries_a_held_view_to_the_disk_without_moving_its_cards() {
    let (r, mut app) = on_a();
    comment_rows(&mut app, 2, 2, "on gamma");
    app.open_list();
    assert!(app.mode.is_modal(), "the list holds the view still");
    r.write("a.rs", &format!("new\n{FIVE}"));
    land_world(&mut app);

    let failing = FakeTarget::new(false);
    assert!(!app.export(&failing));
    assert!(failing.last().starts_with("a.rs:4\n"), "the agent gets today's line numbers");
    assert_eq!(anchor(&app, 0), (3, 3, false), "a failed export moves no card");

    let ok = FakeTarget::new(true);
    assert!(app.export(&ok));
    assert!(ok.last().starts_with("a.rs:4\n"));
}

#[test]
fn a_comment_on_a_file_that_is_gone_stays_where_it_was() {
    let (r, mut app) = on_a();
    comment_rows(&mut app, 3, 3, "on delta");
    r.remove("a.rs");
    land_world(&mut app);
    app.carry_comments();
    assert_eq!(anchor(&app, 0), (4, 4, false), "an absent file is no version of it");
    let c = app.store.get(0).unwrap().clone();
    assert!(app.is_stale(&c));
    r.write("a.rs", FIVE);
    app.carry_comments();
    assert_eq!(anchor(&app, 0), (4, 4, false), "and it returns to its line with the file");
}

#[test]
fn a_save_from_the_built_in_editor_carries_the_files_comments() {
    let (r, mut app) = on_a();
    comment_rows(&mut app, 1, 1, "on beta");
    app.diff_cursor = 0;
    let keys = Keymap::default();
    let key = |app: &mut App, code: KeyCode, m: KeyModifiers| {
        handle_key(app, KeyEvent::new(code, m), AREA, &keys).unwrap();
    };
    key(&mut app, KeyCode::Char('e'), KeyModifiers::NONE);
    let target = app.editor_request.take().expect("`e` names the file");
    app.open_inline_editor(&target);
    assert_eq!(app.mode, Mode::Editing);
    // Two lines above `beta`, the first of them typed over `alpha`'s start.
    for code in [KeyCode::Char('x'), KeyCode::Enter, KeyCode::Char('y'), KeyCode::Enter] {
        key(&mut app, code, KeyModifiers::NONE);
    }
    key(&mut app, KeyCode::Char('s'), KeyModifiers::CONTROL);
    assert_eq!(std::fs::read_to_string(r.path().join("a.rs")).unwrap(), format!("x\ny\n{FIVE}"));
    key(&mut app, KeyCode::Esc, KeyModifiers::NONE);
    assert_eq!(app.mode, Mode::Normal);
    assert_eq!(anchor(&app, 0), (4, 4, false));
    assert_eq!(card_texts(&app), ["beta"]);
}
