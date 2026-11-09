//! The built-in editor end to end: `e` with no `editor` configured opens it over the read
//! pane, keys edit through the real dispatcher, `ctrl+s` writes the worktree, and nothing it
//! holds is lost to a poll, a refresh, or an `esc`.

mod common;

use common::{Repo, app_on, enter_tab, land_world};
use hq_deck::app::{App, Focus, FooterAction, Mode, Tab};
use hq_deck::edit_session::{Confirm, Disk};
use hq_deck::keymap::Keymap;
use hq_deck::{handle_key, handle_mouse};
use ratatui::Terminal;
use ratatui::backend::TestBackend;
use ratatui::crossterm::event::{
    KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind,
};
use ratatui::layout::Rect;

const AREA: Rect = Rect { x: 0, y: 0, width: 120, height: 40 };

/// A repo whose tracked `a.rs` has an uncommitted edit, so it shows in `Changes`.
fn repo() -> Repo {
    let r = Repo::init();
    r.write("a.rs", "alpha\nbeta\ngamma\n");
    r.commit_all("init");
    r.write("a.rs", "alpha\nBETA\ngamma\n");
    r
}

fn key(app: &mut App, code: KeyCode, modifiers: KeyModifiers) {
    handle_key(app, KeyEvent::new(code, modifiers), AREA, &Keymap::default()).unwrap();
}

fn press(app: &mut App, code: KeyCode) {
    key(app, code, KeyModifiers::NONE);
}

fn ctrl(app: &mut App, ch: char) {
    key(app, KeyCode::Char(ch), KeyModifiers::CONTROL);
}

fn type_text(app: &mut App, text: &str) {
    for ch in text.chars() {
        press(app, KeyCode::Char(ch));
    }
}

/// Press `e` and service the request the way the event loop does with no `editor` configured.
fn open_editor(app: &mut App) {
    press(app, KeyCode::Char('e'));
    let target = app.editor_request.take().expect("`e` names a file");
    app.open_inline_editor(&target);
}

fn on_disk(r: &Repo) -> String {
    std::fs::read_to_string(r.path().join("a.rs")).unwrap()
}

fn render(app: &App, width: u16, height: u16) -> String {
    let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
    terminal.draw(|f| hq_deck::ui::render(f, app)).unwrap();
    let buffer = terminal.backend().buffer().clone();
    let mut out = String::new();
    for y in 0..height {
        for x in 0..width {
            out.push_str(buffer.cell((x, y)).unwrap().symbol());
        }
        out.push('\n');
    }
    out
}

fn editor_lines(app: &App) -> Vec<String> {
    app.edit.as_ref().expect("the editor is open").buffer.lines().to_vec()
}

#[test]
fn e_opens_the_editor_at_the_cursors_line_and_ctrl_s_writes_the_file() {
    let r = repo();
    let mut app = app_on(&r);
    enter_tab(&mut app, Tab::AllFiles);
    app.focus = Focus::Diff;
    app.diff_cursor = 1;
    open_editor(&mut app);
    assert_eq!(app.mode, Mode::Editing);
    let session = app.edit.as_ref().unwrap();
    assert_eq!(session.path, "a.rs");
    assert_eq!(session.buffer.cursor().line, 1, "the cursor opens on the line `e` was on");

    press(&mut app, KeyCode::End);
    type_text(&mut app, "!");
    assert_eq!(on_disk(&r), "alpha\nBETA\ngamma\n", "typing writes nothing");
    ctrl(&mut app, 's');
    assert_eq!(on_disk(&r), "alpha\nBETA!\ngamma\n");
    assert_eq!(app.status, "saved a.rs");
    assert!(app.world_request.is_some(), "the save asks the views to catch up");

    press(&mut app, KeyCode::Esc);
    assert_eq!(app.mode, Mode::Normal, "a saved buffer closes without asking");
    assert!(app.edit.is_none());
    assert!(
        app.visible.iter().any(|row| row.text() == "BETA!"),
        "the File view under the editor catches up as it closes"
    );
}

#[test]
fn e_from_a_diff_edits_the_worktree_file_and_returns_to_the_diff() {
    let r = repo();
    let mut app = app_on(&r);
    app.focus = Focus::Diff;
    open_editor(&mut app);
    assert_eq!(app.tab, Tab::Changes, "the editor opens over the tab it was asked from");
    assert_eq!(editor_lines(&app), ["alpha", "BETA", "gamma"], "the worktree text, not the diff");
    ctrl(&mut app, 'a');
    type_text(&mut app, "one");
    press(&mut app, KeyCode::Enter);
    type_text(&mut app, "two");
    ctrl(&mut app, 's');
    press(&mut app, KeyCode::Esc);
    assert_eq!(on_disk(&r), "one\ntwo\n");
    assert_eq!(app.tab, Tab::Changes);
    assert_eq!(app.mode, Mode::Normal);
}

#[test]
fn every_printable_is_text_while_editing() {
    let r = repo();
    let mut app = app_on(&r);
    open_editor(&mut app);
    // `q` quits, `2` switches tabs, `e` edits, `/` searches in Normal mode. Here they type.
    type_text(&mut app, "q2e/");
    assert_eq!(app.mode, Mode::Editing);
    assert_eq!(app.tab, Tab::Changes);
    assert!(!app.should_quit);
    assert_eq!(editor_lines(&app)[0], "q2e/alpha");
}

#[test]
fn esc_with_unsaved_text_asks_and_discard_leaves_the_file_alone() {
    let r = repo();
    let mut app = app_on(&r);
    open_editor(&mut app);
    type_text(&mut app, "x");
    press(&mut app, KeyCode::Esc);
    assert_eq!(app.edit_asking(), Some(Confirm::Close));
    assert_eq!(app.mode, Mode::Editing, "nothing closes before the answer");
    let bands: Vec<FooterAction> = app.footer_bands().iter().map(|&(a, _)| a).collect();
    assert_eq!(
        bands,
        [
            FooterAction::EditAnswerSave,
            FooterAction::EditAnswerCancel,
            FooterAction::EditAnswerDiscard
        ],
        "the footer lists exactly the answers"
    );

    type_text(&mut app, "zq");
    assert_eq!(editor_lines(&app)[0], "xalpha", "keys the question does not offer do nothing");
    press(&mut app, KeyCode::Esc);
    assert_eq!(app.edit_asking(), None, "`esc` keeps editing");
    type_text(&mut app, "y");
    assert_eq!(editor_lines(&app)[0], "xyalpha");

    press(&mut app, KeyCode::Esc);
    press(&mut app, KeyCode::Char('d'));
    assert_eq!(app.mode, Mode::Normal);
    assert_eq!(on_disk(&r), "alpha\nBETA\ngamma\n", "a discard writes nothing");
}

#[test]
fn save_and_close_from_the_question_writes_then_closes() {
    let r = repo();
    let mut app = app_on(&r);
    open_editor(&mut app);
    type_text(&mut app, "x");
    press(&mut app, KeyCode::Esc);
    press(&mut app, KeyCode::Char('s'));
    assert_eq!(on_disk(&r), "xalpha\nBETA\ngamma\n");
    assert_eq!(app.mode, Mode::Normal);
}

#[test]
fn a_save_over_the_agents_change_asks_and_never_overwrites_silently() {
    let r = repo();
    let mut app = app_on(&r);
    open_editor(&mut app);
    type_text(&mut app, "mine ");
    r.write("a.rs", "the agent wrote this\n");
    app.check_edit_disk();
    assert_eq!(app.edit.as_ref().unwrap().disk, Disk::Changed, "the poll notices the write");
    assert_eq!(editor_lines(&app)[0], "mine alpha", "and leaves the buffer alone");

    ctrl(&mut app, 's');
    assert_eq!(app.edit_asking(), Some(Confirm::Conflict { then_close: false }));
    assert_eq!(on_disk(&r), "the agent wrote this\n", "the agent's change survives the save");

    press(&mut app, KeyCode::Char('o'));
    assert_eq!(on_disk(&r), "mine alpha\nBETA\ngamma\n", "`o` overwrites on purpose");
    assert_eq!(app.edit_asking(), None);
    assert_eq!(app.mode, Mode::Editing, "a plain save stays in the editor");
}

#[test]
fn reload_takes_the_agents_version_in_place_of_the_buffer() {
    let r = repo();
    let mut app = app_on(&r);
    open_editor(&mut app);
    type_text(&mut app, "mine ");
    r.write("a.rs", "theirs\n");
    ctrl(&mut app, 's');
    press(&mut app, KeyCode::Char('r'));
    assert_eq!(editor_lines(&app), ["theirs"]);
    assert_eq!(on_disk(&r), "theirs\n");
    assert!(!app.edit.as_ref().unwrap().is_dirty());
}

#[test]
fn a_closing_save_that_conflicts_closes_once_answered() {
    let r = repo();
    let mut app = app_on(&r);
    open_editor(&mut app);
    type_text(&mut app, "x");
    r.write("a.rs", "theirs\n");
    press(&mut app, KeyCode::Esc);
    press(&mut app, KeyCode::Char('s'));
    assert_eq!(app.edit_asking(), Some(Confirm::Conflict { then_close: true }));
    press(&mut app, KeyCode::Char('o'));
    assert_eq!(app.mode, Mode::Normal);
    assert_eq!(on_disk(&r), "xalpha\nBETA\ngamma\n");
}

#[test]
fn a_poll_landing_while_editing_moves_nothing() {
    let r = repo();
    let mut app = app_on(&r);
    app.focus = Focus::Diff;
    open_editor(&mut app);
    press(&mut app, KeyCode::Down);
    type_text(&mut app, "x");
    let (cursor, visible) = (app.diff_cursor, app.visible.len());
    r.write("b.rs", "new file\n");
    app.request_world_refresh(false);
    land_world(&mut app);
    assert_eq!(app.mode, Mode::Editing);
    assert_eq!(editor_lines(&app), ["alpha", "xBETA", "gamma"]);
    assert_eq!(app.edit.as_ref().unwrap().buffer.cursor().line, 1);
    assert_eq!((app.diff_cursor, app.visible.len()), (cursor, visible), "the view under it holds");
}

#[test]
fn undo_redo_and_the_editors_own_register() {
    let r = repo();
    let mut app = app_on(&r);
    open_editor(&mut app);
    type_text(&mut app, "abc");
    ctrl(&mut app, 'z');
    assert_eq!(editor_lines(&app)[0], "alpha");
    ctrl(&mut app, 'y');
    assert_eq!(editor_lines(&app)[0], "abcalpha");
    key(&mut app, KeyCode::Char('z'), KeyModifiers::CONTROL | KeyModifiers::SHIFT);
    assert_eq!(editor_lines(&app)[0], "abcalpha", "nothing left to redo");

    // Copy the line, then paste it from the register as an unreadable clipboard would.
    let copied = app.edit_copy().unwrap();
    assert_eq!(copied, "abcalpha\n");
    press(&mut app, KeyCode::Home);
    app.edit_paste(None);
    assert_eq!(editor_lines(&app)[..2], ["abcalpha", "abcalpha"]);
    // The paste left the cursor at the start of the line it pushed down.
    app.edit_paste(Some("from the clipboard ".into()));
    assert_eq!(
        editor_lines(&app)[1],
        "from the clipboard abcalpha",
        "the clipboard wins when it can be read"
    );
}

#[test]
fn an_unreadable_file_does_not_open_and_says_why() {
    let r = repo();
    r.write("bin.dat", "a\u{0}b");
    let mut app = app_on(&r);
    enter_tab(&mut app, Tab::AllFiles);
    let target = hq_deck::app::EditTarget { path: "bin.dat".into(), line: 1 };
    app.open_inline_editor(&target);
    assert_eq!(app.mode, Mode::Normal);
    assert!(app.edit.is_none());
    assert_eq!(app.status, "can't edit bin.dat: it is a binary file");
}

#[test]
fn a_click_places_the_cursor_and_a_drag_selects() {
    let r = repo();
    let mut app = app_on(&r);
    app.focus = Focus::Diff;
    open_editor(&mut app);
    let (text, _) = hq_deck::ui::edit_text_geometry(AREA, &app).unwrap();
    let at = |kind, column, row| MouseEvent { kind, column, row, modifiers: KeyModifiers::NONE };
    let send = |app: &mut App, event| {
        handle_mouse(app, event, AREA, &[], &Keymap::default(), &hq_deck::export::Clipboard)
            .unwrap();
    };
    send(&mut app, at(MouseEventKind::Down(MouseButton::Left), text.x + 2, text.y + 1));
    let session = app.edit.as_ref().unwrap();
    assert_eq!((session.buffer.cursor().line, session.buffer.cursor().col), (1, 2));
    send(&mut app, at(MouseEventKind::Drag(MouseButton::Left), text.x + 3, text.y + 2));
    send(&mut app, at(MouseEventKind::Up(MouseButton::Left), text.x + 3, text.y + 2));
    let selection = app.edit.as_ref().unwrap().buffer.selection().unwrap();
    assert_eq!(
        (selection.0.line, selection.0.col, selection.1.line, selection.1.col),
        (1, 2, 2, 3)
    );
}

#[test]
fn the_editor_paints_over_the_read_pane_with_its_own_bar() {
    let r = repo();
    let mut app = app_on(&r);
    app.focus = Focus::Diff;
    open_editor(&mut app);
    type_text(&mut app, "x");
    app.settle_edit_view(30, 60);
    let out = render(&app, 120, 40);
    assert!(out.contains("a.rs · editing ●"), "the title names the file and its unsaved state");
    assert!(out.contains("xalpha"), "the text is on screen");
    assert!(out.contains("^S save"), "the footer is the editor's own");

    r.write("a.rs", "theirs\n");
    app.check_edit_disk();
    let out = render(&app, 120, 40);
    assert!(out.contains("changed on disk"), "the title warns before a save has to ask");

    press(&mut app, KeyCode::Esc);
    let out = render(&app, 120, 40);
    assert!(out.contains("unsaved changes"), "the question is boxed over the pane");
    assert!(out.contains("s save and close"));
}

#[test]
fn the_blocked_config_screen_says_its_quit_discards_an_unsaved_edit() {
    let r = repo();
    let mut app = app_on(&r);
    open_editor(&mut app);
    app.set_config_error("bad key".into());
    let clean = render(&app, 120, 20);
    assert!(!clean.contains("unsaved"), "a clean buffer has nothing to warn about");
    app.edit_act(hq_deck::edit_session::EditAction::Type('x'));
    let dirty = render(&app, 120, 20);
    assert!(dirty.contains("a.rs has unsaved changes in the editor"));
    assert!(dirty.contains("`q` quits and discards them"));
}

#[test]
fn the_editor_takes_the_whole_body_where_scm_mode_has_no_read_pane() {
    let r = repo();
    let mut app = app_on(&r);
    press(&mut app, KeyCode::Char('x'));
    assert!(app.scm_active(), "`x` flips the navigator to SCM mode");
    let target = hq_deck::app::EditTarget { path: "a.rs".into(), line: 1 };
    app.open_inline_editor(&target);
    let narrow = Rect::new(0, 0, 50, 20);
    let (text, gutter) = hq_deck::ui::edit_text_geometry(narrow, &app).unwrap();
    assert_eq!(text.x as usize, 1 + gutter, "the text starts right of the border and gutter");
    assert_eq!(text.width as usize, 50 - 2 - gutter, "across the whole body");
    let out = render(&app, 50, 20);
    assert!(out.contains("alpha"));
}

/// The editor open on a `find.rs` holding three `foo`s, at its first line.
fn editing_three_foos() -> (Repo, App) {
    let r = repo();
    r.write("find.rs", "foo bar\nbaz foo\nfoo\n");
    let mut app = app_on(&r);
    app.open_inline_editor(&hq_deck::app::EditTarget { path: "find.rs".into(), line: 1 });
    assert_eq!(app.mode, Mode::Editing);
    (r, app)
}

/// The selection as `(line, col, line, col)`.
fn selected(app: &App) -> Option<(usize, usize, usize, usize)> {
    let (a, b) = app.edit.as_ref()?.buffer.selection()?;
    Some((a.line, a.col, b.line, b.col))
}

fn find_count(app: &App) -> Option<(Option<usize>, usize)> {
    app.edit.as_ref()?.find_count()
}

#[test]
fn ctrl_f_steps_through_the_matches_and_esc_returns_to_editing() {
    let (_r, mut app) = editing_three_foos();
    ctrl(&mut app, 'f');
    assert!(app.edit_finding());
    type_text(&mut app, "foo");
    assert_eq!(find_count(&app), Some((None, 3)), "typing counts, the cursor stays");
    assert_eq!(selected(&app), None);

    press(&mut app, KeyCode::Enter);
    assert_eq!(selected(&app), Some((0, 0, 0, 3)));
    assert_eq!(find_count(&app), Some((Some(1), 3)));
    press(&mut app, KeyCode::Down);
    assert_eq!(selected(&app), Some((1, 4, 1, 7)));
    press(&mut app, KeyCode::Enter);
    assert_eq!(selected(&app), Some((2, 0, 2, 3)));
    press(&mut app, KeyCode::Enter);
    assert_eq!(selected(&app), Some((0, 0, 0, 3)), "the steps wrap forward");
    press(&mut app, KeyCode::Up);
    assert_eq!(selected(&app), Some((2, 0, 2, 3)), "and back");
    assert_eq!(app.edit.as_ref().unwrap().buffer.cursor().line, 2, "the cursor is on the match");

    press(&mut app, KeyCode::Esc);
    assert!(!app.edit_finding());
    assert_eq!(app.mode, Mode::Editing, "`esc` closes the band, not the editor");
    type_text(&mut app, "X");
    assert_eq!(editor_lines(&app), ["foo bar", "baz foo", "X"], "typing replaces the match");
}

#[test]
fn the_find_band_holds_the_buffer_still() {
    let (_r, mut app) = editing_three_foos();
    ctrl(&mut app, 'f');
    // Every key that would edit the text edits the query, or nothing.
    type_text(&mut app, "q2e");
    for code in [KeyCode::Backspace, KeyCode::Tab, KeyCode::Delete] {
        press(&mut app, code);
    }
    for ch in ['x', 'v', 'z', 'y', 'd', 'k', 's'] {
        ctrl(&mut app, ch);
    }
    app.input_paste("pasted\ntext");
    assert_eq!(editor_lines(&app), ["foo bar", "baz foo", "foo"]);
    let session = app.edit.as_ref().unwrap();
    assert!(!session.is_dirty());
    assert_eq!(
        session.find.as_ref().unwrap().query,
        "q2pasted text",
        "the query took the keys, and a paste as one line"
    );
    // An edit reaching the session another way is dropped too.
    app.edit_act(hq_deck::edit_session::EditAction::Type('!'));
    assert_eq!(editor_lines(&app)[0], "foo bar");
}

#[test]
fn the_find_band_is_smart_case_and_counts_no_matches() {
    let (_r, mut app) = editing_three_foos();
    ctrl(&mut app, 'f');
    type_text(&mut app, "Foo");
    assert_eq!(find_count(&app), Some((None, 0)), "an uppercase letter makes it case-sensitive");
    let bands: Vec<FooterAction> = app.footer_bands().iter().map(|&(a, _)| a).collect();
    assert_eq!(bands, [FooterAction::CloseFind], "no step is offered with nothing to step to");
    for _ in 0..3 {
        press(&mut app, KeyCode::Backspace);
    }
    type_text(&mut app, "foo");
    let bands: Vec<FooterAction> = app.footer_bands().iter().map(|&(a, _)| a).collect();
    assert_eq!(bands, [FooterAction::FindStep, FooterAction::CloseFind]);
}

#[test]
fn the_find_band_paints_at_the_foot_of_the_editor_with_the_matches_marked() {
    let (_r, mut app) = editing_three_foos();
    app.settle_edit_view(30, 60);
    let out = render(&app, 120, 40);
    assert!(out.contains("^F find"), "the editor's bar offers find");
    ctrl(&mut app, 'f');
    type_text(&mut app, "foo");
    press(&mut app, KeyCode::Enter);
    let mut terminal = Terminal::new(TestBackend::new(120, 40)).unwrap();
    terminal.draw(|f| hq_deck::ui::render(f, &app)).unwrap();
    let buffer = terminal.backend().buffer().clone();
    let (text, _) = hq_deck::ui::edit_text_geometry(AREA, &app).unwrap();
    let row = |y: u16| -> String {
        (text.x..text.x + text.width).map(|x| buffer.cell((x, y)).unwrap().symbol()).collect()
    };
    let band = row(text.y + text.height);
    assert!(band.contains("1/3"), "the band's count, on the row under the text: {band:?}");
    let current = buffer.cell((text.x, text.y)).unwrap().bg;
    let other = buffer.cell((text.x + 4, text.y + 1)).unwrap().bg;
    let plain = buffer.cell((text.x + 4, text.y)).unwrap().bg;
    assert_ne!(current, other, "the current match stands out from the others");
    assert_ne!(other, plain, "every match is marked");
    let out = render(&app, 120, 40);
    assert!(out.contains("↑↓ move"), "the footer is the band's");
}

#[test]
fn a_double_click_selects_the_word_and_a_triple_click_the_line() {
    let (_r, mut app) = editing_three_foos();
    let (text, _) = hq_deck::ui::edit_text_geometry(AREA, &app).unwrap();
    let at = |kind, column, row| MouseEvent { kind, column, row, modifiers: KeyModifiers::NONE };
    let click = |app: &mut App, column, row| {
        for kind in [MouseEventKind::Down(MouseButton::Left), MouseEventKind::Up(MouseButton::Left)]
        {
            handle_mouse(
                app,
                at(kind, column, row),
                AREA,
                &[],
                &Keymap::default(),
                &hq_deck::export::Clipboard,
            )
            .unwrap();
        }
    };
    // `baz foo`, on the `o` of `foo`.
    click(&mut app, text.x + 5, text.y + 1);
    assert_eq!(selected(&app), None);
    click(&mut app, text.x + 5, text.y + 1);
    assert_eq!(selected(&app), Some((1, 4, 1, 7)), "a double click takes `foo`");
    click(&mut app, text.x + 5, text.y + 1);
    assert_eq!(selected(&app), Some((1, 0, 2, 0)), "a triple click takes the line and its break");
    click(&mut app, text.x + 1, text.y);
    assert_eq!(selected(&app), None, "a click elsewhere starts over");
}
