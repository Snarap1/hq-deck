//! The built-in editor's session: one file open for editing over the read pane.
//!
//! `edit` opens one when the `editor` config key is unset. The session owns the buffer
//! (`src/edit_buffer.rs`), its incremental highlight, its own scroll, and what it last knew of
//! the file on disk. `App` holds one field (`edit`) and delegating methods; keep editor state
//! here, not in `app.rs`.
//!
//! The only write is [`EditSession::save`], made by `ctrl+s` (Writes are asked for). It never
//! overwrites a file that changed on disk since it was read or saved without asking first: the
//! agent edits the same worktree, and its change must not vanish under a save.

use std::cell::RefCell;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::SystemTime;

use crate::edit_buffer::{Buffer, Pos};
use crate::highlight::{Highlighter, LineHighlights};

thread_local! {
    /// Each open session's per-line highlight, by session key.
    ///
    /// Kept out of the session itself because syntect's parser state is `!Send` (it holds
    /// oniguruma regions) and `App` must be `Send`: config recovery builds the replacement on a
    /// worker. The cache is purely derived from the buffer, so a missing entry is rebuilt from
    /// the text and nothing depends on which thread holds it.
    static HIGHLIGHTS: RefCell<HashMap<u64, LineHighlights>> = RefCell::new(HashMap::new());
}

/// The next session's key into [`HIGHLIGHTS`].
static NEXT_KEY: AtomicU64 = AtomicU64::new(1);

/// The rows kept between the cursor and the viewport's edge when a move scrolls.
const SCROLL_MARGIN: usize = 2;

/// A question the session is waiting on, answered by one key.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Confirm {
    /// Closing with unsaved changes: `s` save and close, `d` discard, `esc` keep editing.
    Close,
    /// The file changed on disk since it was read: `o` overwrite, `r` reload the disk's
    /// version, `esc` keep editing. `then_close` remembers that the save was a closing one.
    Conflict { then_close: bool },
}

/// What the session knows the file on disk to be, compared with what it read or last saved.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Disk {
    /// The file holds what the session read or saved.
    Same,
    /// Something else wrote the file since.
    Changed,
    /// The file is gone.
    Gone,
}

/// The outcome of a save.
#[derive(Debug, PartialEq, Eq)]
pub enum Saved {
    Written,
    /// The file changed on disk, and a save without `force` does not overwrite it.
    Conflict,
    Failed(String),
}

/// One editing gesture, already resolved from the key or click that made it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum EditAction {
    Type(char),
    Paste(String),
    Newline,
    Tab,
    BackTab,
    Backspace,
    Delete,
    DeleteWordBack,
    Left {
        word: bool,
        extend: bool,
    },
    Right {
        word: bool,
        extend: bool,
    },
    Up {
        extend: bool,
    },
    Down {
        extend: bool,
    },
    PageUp {
        extend: bool,
    },
    PageDown {
        extend: bool,
    },
    Home {
        extend: bool,
    },
    End {
        extend: bool,
    },
    DocStart {
        extend: bool,
    },
    DocEnd {
        extend: bool,
    },
    SelectAll,
    Undo,
    Redo,
    Duplicate,
    /// A click (or a drag, extending) at a row of the viewport and a display column of the text.
    Click {
        row: usize,
        col: usize,
        extend: bool,
    },
    /// The wheel: scroll the view by rows, leaving the cursor where it is.
    Scroll(isize),
}

/// The file's identity on disk at a point in time: cheap to read on every poll, and enough to
/// skip reading the bytes when nothing touched the file.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Stamp {
    len: u64,
    modified: Option<SystemTime>,
}

fn stamp(path: &Path) -> Option<Stamp> {
    let meta = std::fs::metadata(path).ok()?;
    Some(Stamp { len: meta.len(), modified: meta.modified().ok() })
}

/// The open file and everything the editor shows of it.
#[derive(Debug)]
pub struct EditSession {
    /// Repository-relative, as the navigator names it.
    pub path: String,
    abs: PathBuf,
    pub buffer: Buffer,
    /// This session's entry in [`HIGHLIGHTS`], filled by the renderer for the rows it paints.
    highlight_key: u64,
    /// The file's extension, which picks its grammar.
    language: Option<String>,
    /// The top visible line.
    pub scroll: usize,
    /// The display column at the left edge of the text.
    pub h_scroll: usize,
    /// Rows and text columns of the viewport, noted each frame for paging and scrolling.
    viewport: (usize, usize),
    /// A move asked for the cursor to be scrolled into view on the next settle.
    reveal: bool,
    /// The bytes on disk when the session last read or wrote them.
    disk_bytes: Vec<u8>,
    disk_stamp: Option<Stamp>,
    pub disk: Disk,
    pub confirm: Option<Confirm>,
    /// A left-button drag that started in the text, extending the selection until release.
    pub drag: bool,
}

impl EditSession {
    /// Open `path` (repository-relative, under `repo`) with the cursor on `line` (1-based).
    /// The error says why the file cannot be edited here.
    pub fn open(repo: &Path, path: &str, line: u32) -> Result<Self, String> {
        let abs = repo.join(path);
        let meta = std::fs::metadata(&abs).map_err(|_| "it is gone".to_owned())?;
        if !meta.is_file() {
            return Err("it is not a file".to_owned());
        }
        // The same budget the read pane holds files to: past it the view is a notice, and a
        // buffer of it would make every keypress pay for the whole file.
        if crate::diff::over_byte_budget(meta.len() as usize) {
            return Err("it is too large".to_owned());
        }
        let bytes = std::fs::read(&abs).map_err(|e| format!("reading failed: {e}"))?;
        let mut buffer = Buffer::parse(&bytes).map_err(|why| format!("it is {}", why.reason()))?;
        buffer.place_at_line(line.saturating_sub(1) as usize);
        Ok(Self {
            path: path.to_owned(),
            disk_stamp: stamp(&abs),
            abs,
            buffer,
            highlight_key: NEXT_KEY.fetch_add(1, Ordering::Relaxed),
            language: crate::diff::language_of(path),
            scroll: 0,
            h_scroll: 0,
            viewport: (0, 0),
            reveal: true,
            disk_bytes: bytes,
            disk: Disk::Same,
            confirm: None,
            drag: false,
        })
    }

    pub fn is_dirty(&self) -> bool {
        self.buffer.is_dirty()
    }

    /// Apply one gesture to the buffer and keep the highlight aligned with the text.
    pub fn act(&mut self, action: EditAction) {
        let b = &mut self.buffer;
        let page = self.viewport.0.saturating_sub(1).max(1);
        let mut reveal = true;
        match action {
            EditAction::Type(ch) => b.type_char(ch),
            EditAction::Paste(text) => b.insert(&text),
            EditAction::Newline => b.newline(),
            EditAction::Tab => b.tab(),
            EditAction::BackTab => b.backtab(),
            EditAction::Backspace => b.backspace(),
            EditAction::Delete => b.delete(),
            EditAction::DeleteWordBack => b.delete_word_back(),
            EditAction::Left { word, extend } => b.left(word, extend),
            EditAction::Right { word, extend } => b.right(word, extend),
            EditAction::Up { extend } => b.up(1, extend),
            EditAction::Down { extend } => b.down(1, extend),
            EditAction::PageUp { extend } => {
                b.up(page, extend);
                self.scroll = self.scroll.saturating_sub(page);
            }
            EditAction::PageDown { extend } => {
                b.down(page, extend);
                self.scroll += page;
            }
            EditAction::Home { extend } => b.home(extend),
            EditAction::End { extend } => b.end(extend),
            EditAction::DocStart { extend } => b.doc_start(extend),
            EditAction::DocEnd { extend } => b.doc_end(extend),
            EditAction::SelectAll => b.select_all(),
            EditAction::Undo => {
                b.undo();
            }
            EditAction::Redo => {
                b.redo();
            }
            EditAction::Duplicate => b.duplicate(),
            EditAction::Click { row, col, extend } => {
                b.click(self.scroll + row, self.h_scroll + col, extend);
            }
            EditAction::Scroll(delta) => {
                self.scroll = self.scroll.saturating_add_signed(delta);
                reveal = false;
            }
        }
        self.reveal |= reveal;
        self.sync_highlight();
    }

    /// `copy`: the selection, or the cursor's line.
    pub fn copy(&self) -> String {
        self.buffer.copy_text()
    }

    /// `cut`: remove and return the selection, or the cursor's line.
    pub fn cut(&mut self) -> String {
        let text = self.buffer.cut();
        self.reveal = true;
        self.sync_highlight();
        text
    }

    /// Bring the scroll in line with the viewport the frame is about to paint: the cursor into
    /// view when a move asked for it, and the offsets within the text always.
    pub fn settle(&mut self, rows: usize, cols: usize) {
        self.viewport = (rows, cols);
        let last = self.buffer.line_count().saturating_sub(1);
        if std::mem::take(&mut self.reveal) && rows > 0 {
            let line = self.buffer.cursor().line;
            let margin = SCROLL_MARGIN.min(rows.saturating_sub(1) / 2);
            if line < self.scroll + margin {
                self.scroll = line.saturating_sub(margin);
            } else if line + margin >= self.scroll + rows {
                self.scroll = (line + margin + 1).saturating_sub(rows);
            }
            if cols > 0 {
                let col = self.buffer.display_col(self.buffer.cursor());
                if col < self.h_scroll {
                    self.h_scroll = col;
                } else if col >= self.h_scroll + cols {
                    self.h_scroll = col + 1 - cols;
                }
            }
        }
        self.scroll = self.scroll.min(last);
    }

    /// Highlight the lines up to `upto` against the current text, then hand the highlight to
    /// `read`. `read` must not reach back into another session's highlight.
    pub fn with_highlight<R>(
        &self,
        hl: &Highlighter,
        upto: usize,
        read: impl FnOnce(&LineHighlights) -> R,
    ) -> R {
        HIGHLIGHTS.with(|all| {
            let mut all = all.borrow_mut();
            let lines = self.buffer.lines();
            let fresh = || LineHighlights::new(self.language.as_deref(), lines.len());
            let highlight = all.entry(self.highlight_key).or_insert_with(fresh);
            // Splices keep the cache aligned by construction; a mismatch could only come from a
            // bug, and the derived cache answers it by starting over rather than painting
            // another line's colors.
            if highlight.line_count() != lines.len() {
                *highlight = fresh();
            }
            highlight.ensure(hl, lines, upto);
            read(highlight)
        })
    }

    /// Forget the highlight, as a theme change requires.
    pub fn clear_highlight(&mut self) {
        self.drop_highlight();
    }

    fn drop_highlight(&self) {
        // `try_with`: a session dropped while the thread tears down its locals has nothing left
        // to clean.
        let _ = HIGHLIGHTS.try_with(|all| all.borrow_mut().remove(&self.highlight_key));
    }

    /// Compare the file on disk with what the session read or saved. Called on the poll, so a
    /// file nothing touched costs one `stat`.
    pub fn check_disk(&mut self) {
        let now = stamp(&self.abs);
        if now.is_none() {
            self.disk = Disk::Gone;
            return;
        }
        if now == self.disk_stamp {
            return;
        }
        match std::fs::read(&self.abs) {
            Ok(bytes) if bytes == self.disk_bytes => {
                // Touched but not changed: remember the new stamp so the next poll is a `stat`.
                self.disk_stamp = now;
                self.disk = Disk::Same;
            }
            Ok(_) => self.disk = Disk::Changed,
            Err(_) => self.disk = Disk::Gone,
        }
    }

    /// Write the buffer to the file. Without `force`, a file that changed on disk (or is gone)
    /// since the session read or saved it is left alone and the save reports the conflict.
    ///
    /// The write goes through the path in place, not by rename: the file keeps its inode, its
    /// permissions, and its owner, and a symlink keeps pointing where it did.
    pub fn save(&mut self, force: bool) -> Saved {
        if !force {
            match std::fs::read(&self.abs) {
                Ok(bytes) if bytes == self.disk_bytes => {}
                _ => {
                    self.check_disk();
                    if self.disk == Disk::Same {
                        // The read failed for a reason that did not survive a second look.
                        self.disk = Disk::Changed;
                    }
                    return Saved::Conflict;
                }
            }
        }
        let text = self.buffer.text();
        if let Err(e) = std::fs::write(&self.abs, text.as_bytes()) {
            return Saved::Failed(e.to_string());
        }
        self.disk_bytes = text.into_bytes();
        self.disk_stamp = stamp(&self.abs);
        self.disk = Disk::Same;
        self.buffer.mark_saved();
        Saved::Written
    }

    /// Replace the buffer with the file's version on disk, dropping every unsaved change and
    /// the history. The cursor stays on its line, clamped.
    pub fn reload(&mut self) -> Result<(), String> {
        let bytes = std::fs::read(&self.abs).map_err(|_| "it is gone".to_owned())?;
        let mut buffer = Buffer::parse(&bytes).map_err(|why| format!("it is {}", why.reason()))?;
        let Pos { line, .. } = self.buffer.cursor();
        buffer.place_at_line(line);
        self.drop_highlight();
        self.buffer = buffer;
        self.disk_stamp = stamp(&self.abs);
        self.disk_bytes = bytes;
        self.disk = Disk::Same;
        self.reveal = true;
        Ok(())
    }

    /// Replay the buffer's line splices onto the highlight, keeping it aligned with the text.
    /// Without an entry on this thread there is nothing to align, and the next paint builds one.
    fn sync_highlight(&mut self) {
        let splices = self.buffer.take_splices();
        let _ = HIGHLIGHTS.try_with(|all| {
            if let Some(highlight) = all.borrow_mut().get_mut(&self.highlight_key) {
                for splice in splices {
                    highlight.splice(splice);
                }
            }
        });
    }
}

impl Drop for EditSession {
    fn drop(&mut self) {
        self.drop_highlight();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn repo_with(name: &str, content: &str) -> (tempfile::TempDir, EditSession) {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join(name), content).unwrap();
        let session = EditSession::open(dir.path(), name, 1).unwrap();
        (dir, session)
    }

    fn read(dir: &tempfile::TempDir, name: &str) -> String {
        std::fs::read_to_string(dir.path().join(name)).unwrap()
    }

    /// `n` numbered lines, each `{prefix}{i}`, with a final newline.
    fn numbered(n: usize, prefix: &str) -> String {
        let lines: Vec<String> = (0..n).map(|i| format!("{prefix}{i}")).collect();
        format!("{}\n", lines.join("\n"))
    }

    #[test]
    fn a_session_and_the_app_holding_it_stay_send() {
        // Config recovery builds the replacement `App` on a worker and sends it back, so the
        // session must never hold syntect's `!Send` parser state itself.
        fn assert_send<T: Send>() {}
        assert_send::<EditSession>();
        assert_send::<crate::app::App>();
    }

    #[test]
    fn the_highlight_follows_every_edit_and_reload() {
        let h = Highlighter::new(crate::theme::resolve(Some("catppuccin")).syntax);
        let (dir, mut s) = repo_with("a.rs", "fn a() {}\nlet x = 1;\n");
        let painted = |s: &EditSession| -> Vec<String> {
            s.with_highlight(&h, usize::MAX, |hl| {
                (0..s.buffer.line_count())
                    .map(|i| hl.spans(i).unwrap().iter().map(|sp| sp.text.as_str()).collect())
                    .collect()
            })
        };
        assert_eq!(painted(&s), ["fn a() {}", "let x = 1;"]);
        s.act(EditAction::Down { extend: false });
        s.act(EditAction::Paste("// one\n// two\n".into()));
        assert_eq!(painted(&s), ["fn a() {}", "// one", "// two", "let x = 1;"]);
        s.act(EditAction::Undo);
        assert_eq!(painted(&s), ["fn a() {}", "let x = 1;"]);
        std::fs::write(dir.path().join("a.rs"), "struct S;\n").unwrap();
        s.reload().unwrap();
        assert_eq!(painted(&s), ["struct S;"], "a reload starts the highlight over");
    }

    #[test]
    fn opening_places_the_cursor_on_the_requested_line() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("a.txt"), "one\ntwo\nthree\n").unwrap();
        let s = EditSession::open(dir.path(), "a.txt", 2).unwrap();
        assert_eq!(s.buffer.cursor(), Pos::new(1, 0));
        let past = EditSession::open(dir.path(), "a.txt", 99).unwrap();
        assert_eq!(past.buffer.cursor(), Pos::new(2, 0), "a line past the end clamps");
    }

    #[test]
    fn what_cannot_be_edited_is_refused_with_a_reason() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("bin"), b"a\0b").unwrap();
        std::fs::create_dir(dir.path().join("dir")).unwrap();
        let why = |name: &str| EditSession::open(dir.path(), name, 1).unwrap_err();
        assert_eq!(why("bin"), "it is a binary file");
        assert_eq!(why("dir"), "it is not a file");
        assert_eq!(why("missing"), "it is gone");
    }

    #[test]
    fn a_save_writes_the_buffer_and_makes_it_clean() {
        let (dir, mut s) = repo_with("a.txt", "hello\r\n");
        s.act(EditAction::End { extend: false });
        s.act(EditAction::Type('!'));
        assert!(s.is_dirty());
        assert_eq!(read(&dir, "a.txt"), "hello\r\n", "nothing is written before the save");
        assert_eq!(s.save(false), Saved::Written);
        assert_eq!(read(&dir, "a.txt"), "hello!\r\n", "the file keeps its line ending");
        assert!(!s.is_dirty());
        s.act(EditAction::Type('?'));
        assert_eq!(
            s.save(false),
            Saved::Written,
            "a second save over its own write is no conflict"
        );
        assert_eq!(read(&dir, "a.txt"), "hello!?\r\n");
    }

    #[test]
    fn a_save_never_overwrites_a_change_made_on_disk_without_force() {
        let (dir, mut s) = repo_with("a.txt", "mine\n");
        s.act(EditAction::Type('x'));
        std::fs::write(dir.path().join("a.txt"), "the agent's\n").unwrap();
        assert_eq!(s.save(false), Saved::Conflict);
        assert_eq!(read(&dir, "a.txt"), "the agent's\n", "the agent's change survives");
        assert_eq!(s.disk, Disk::Changed);
        assert_eq!(s.save(true), Saved::Written, "an explicit overwrite writes");
        assert_eq!(read(&dir, "a.txt"), "xmine\n");
        assert_eq!(s.disk, Disk::Same);
    }

    #[test]
    fn a_removed_file_is_a_conflict_too() {
        let (dir, mut s) = repo_with("a.txt", "a\n");
        std::fs::remove_file(dir.path().join("a.txt")).unwrap();
        assert_eq!(s.save(false), Saved::Conflict);
        assert_eq!(s.disk, Disk::Gone);
        assert!(!dir.path().join("a.txt").exists(), "nothing is recreated without asking");
    }

    #[test]
    fn the_disk_check_tells_a_change_from_a_touch() {
        let (dir, mut s) = repo_with("a.txt", "a\n");
        s.check_disk();
        assert_eq!(s.disk, Disk::Same);
        // Same bytes, new stamp (the length is the same, so force the mtime forward).
        let file = std::fs::File::options().write(true).open(dir.path().join("a.txt")).unwrap();
        file.set_modified(SystemTime::now() + std::time::Duration::from_secs(5)).unwrap();
        s.check_disk();
        assert_eq!(s.disk, Disk::Same, "a touch is not a change");
        std::fs::write(dir.path().join("a.txt"), "b\n").unwrap();
        s.check_disk();
        assert_eq!(s.disk, Disk::Changed);
        std::fs::remove_file(dir.path().join("a.txt")).unwrap();
        s.check_disk();
        assert_eq!(s.disk, Disk::Gone);
    }

    #[test]
    fn reload_takes_the_disks_version_and_drops_the_history() {
        let (dir, mut s) = repo_with("a.txt", "one\ntwo\n");
        s.act(EditAction::Down { extend: false });
        s.act(EditAction::Type('x'));
        std::fs::write(dir.path().join("a.txt"), "uno\ndos\ntres\n").unwrap();
        s.check_disk();
        s.reload().unwrap();
        assert_eq!(s.buffer.lines(), ["uno", "dos", "tres"]);
        assert_eq!(s.buffer.cursor(), Pos::new(1, 0), "the cursor keeps its line");
        assert!(!s.is_dirty());
        assert_eq!(s.disk, Disk::Same);
        s.act(EditAction::Undo);
        assert_eq!(s.buffer.lines(), ["uno", "dos", "tres"], "nothing to undo past a reload");
    }

    #[test]
    fn a_save_through_a_symlink_writes_its_target_and_keeps_the_link() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("real.txt"), "a\n").unwrap();
        std::os::unix::fs::symlink("real.txt", dir.path().join("link.txt")).unwrap();
        let mut s = EditSession::open(dir.path(), "link.txt", 1).unwrap();
        s.act(EditAction::Type('b'));
        assert_eq!(s.save(false), Saved::Written);
        assert!(dir.path().join("link.txt").symlink_metadata().unwrap().file_type().is_symlink());
        assert_eq!(read(&dir, "real.txt"), "ba\n");
    }

    #[test]
    fn settle_scrolls_the_cursor_into_view_with_a_margin() {
        let text = numbered(100, "");
        let (_dir, mut s) = repo_with("a.txt", &text);
        s.settle(10, 40);
        assert_eq!(s.scroll, 0);
        for _ in 0..20 {
            s.act(EditAction::Down { extend: false });
        }
        s.settle(10, 40);
        assert_eq!(s.scroll, 13, "line 20 sits two rows above the bottom edge");
        s.act(EditAction::Scroll(-5));
        s.settle(10, 40);
        assert_eq!(s.scroll, 8, "the wheel scrolls without pulling the view back");
        s.act(EditAction::Scroll(1000));
        s.settle(10, 40);
        assert_eq!(s.scroll, 99, "and stops at the last line");
    }

    #[test]
    fn settle_scrolls_horizontally_to_keep_the_cursor_visible() {
        let (_dir, mut s) = repo_with("a.txt", &format!("{}\n", "x".repeat(100)));
        s.act(EditAction::End { extend: false });
        s.settle(10, 40);
        assert_eq!(s.h_scroll, 61, "the cursor's cell is the last visible column");
        s.act(EditAction::Home { extend: false });
        s.settle(10, 40);
        assert_eq!(s.h_scroll, 0);
    }

    #[test]
    fn a_click_lands_at_the_scrolled_cell() {
        let text = numbered(50, "line ");
        let (_dir, mut s) = repo_with("a.txt", &text);
        s.scroll = 10;
        s.act(EditAction::Click { row: 3, col: 5, extend: false });
        assert_eq!(s.buffer.cursor(), Pos::new(13, 5));
        s.act(EditAction::Click { row: 4, col: 2, extend: true });
        assert_eq!(s.buffer.selection(), Some((Pos::new(13, 5), Pos::new(14, 2))));
    }

    #[test]
    fn page_moves_take_the_viewport_height() {
        let text = numbered(100, "");
        let (_dir, mut s) = repo_with("a.txt", &text);
        s.settle(10, 40);
        s.act(EditAction::PageDown { extend: false });
        assert_eq!(s.buffer.cursor().line, 9);
        s.act(EditAction::PageUp { extend: false });
        assert_eq!(s.buffer.cursor().line, 0);
    }
}
