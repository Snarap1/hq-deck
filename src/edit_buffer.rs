//! The built-in editor's text: its lines, the cursor and selection, undo and redo, and the
//! format the file was read in.
//!
//! Pure: no terminal and no filesystem. `App` reads the file and writes it back, and the
//! renderer paints [`Buffer::lines`] through the incremental highlighter. Every mutation goes
//! through one primitive, `Buffer::replace`, which is what keeps undo and the highlighter's
//! line splices exact by construction.
//!
//! Columns are char indices into a line. Display columns (tabs expanded to [`TAB_WIDTH`], wide
//! characters counted twice) exist only for vertical moves, clicks, and the renderer.

use unicode_width::UnicodeWidthChar;

/// The display width of a tab stop. The read pane expands tabs to the same width, so a file
/// lines up the same in the File view and in the editor.
pub const TAB_WIDTH: usize = 4;

/// A place in the buffer: a line index and a char column within it.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord)]
pub struct Pos {
    pub line: usize,
    pub col: usize,
}

impl Pos {
    pub const fn new(line: usize, col: usize) -> Self {
        Self { line, col }
    }
}

/// How the file separates its lines. A save writes back the same separator it read.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LineEnding {
    Lf,
    CrLf,
}

impl LineEnding {
    fn as_str(self) -> &'static str {
        match self {
            LineEnding::Lf => "\n",
            LineEnding::CrLf => "\r\n",
        }
    }
}

/// What one press of `tab` inserts, read off the file's own indentation.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Indent {
    Tab,
    Spaces(usize),
}

/// Why a file cannot be edited here. Each would be corrupted by a round trip through a buffer
/// of `String` lines with one line separator, so the editor refuses rather than guessing.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Unreadable {
    /// The bytes are not UTF-8.
    NotUtf8,
    /// A NUL byte: the file is binary.
    Binary,
    /// Both `\n` and `\r\n` separate lines, and a save would have to pick one.
    MixedLineEndings,
}

impl Unreadable {
    pub fn reason(self) -> &'static str {
        match self {
            Unreadable::NotUtf8 => "not UTF-8 text",
            Unreadable::Binary => "a binary file",
            Unreadable::MixedLineEndings => "mixed line endings",
        }
    }
}

/// One edit's effect on the line array: the `removed` lines from `start` were replaced by
/// `inserted` new ones. Consumers that cache per-line state (the highlighter) replay these to
/// stay aligned with the text.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Splice {
    pub start: usize,
    pub removed: usize,
    pub inserted: usize,
}

/// One undoable step: the text `removed` from `start` and the text `inserted` in its place,
/// with the cursor and selection on either side of it.
#[derive(Clone, Debug)]
struct Step {
    /// Identifies the buffer state this step produces, so a save can name the state it wrote.
    id: u64,
    start: Pos,
    removed: String,
    inserted: String,
    cursor_before: Pos,
    anchor_before: Option<Pos>,
    cursor_after: Pos,
}

/// The text being edited, with its cursor, selection, and history.
#[derive(Clone, Debug)]
pub struct Buffer {
    /// Never empty: an empty file is one empty line.
    lines: Vec<String>,
    ending: LineEnding,
    /// Whether the file ended with a line separator, kept so a save does not add or drop one.
    final_newline: bool,
    indent: Indent,
    cursor: Pos,
    /// The selection's fixed end. The selection is `anchor..cursor` in either order, and an
    /// anchor equal to the cursor selects nothing.
    anchor: Option<Pos>,
    /// The display column a run of vertical moves aims for, so passing a short line does not
    /// pull the cursor left for good.
    goal: Option<usize>,
    undo: Vec<Step>,
    redo: Vec<Step>,
    next_id: u64,
    /// The id of the state on disk: the top undo step's id when it was saved, `0` for the state
    /// the buffer was read in.
    saved: u64,
    /// Whether the last edit was typed text that the next typed character may join, so undo
    /// takes back a word rather than one character.
    typing: bool,
    splices: Vec<Splice>,
}

impl Buffer {
    /// Read a file's bytes into a buffer, refusing what a save could not write back unchanged.
    pub fn parse(bytes: &[u8]) -> Result<Self, Unreadable> {
        if bytes.contains(&0) {
            return Err(Unreadable::Binary);
        }
        let text = std::str::from_utf8(bytes).map_err(|_| Unreadable::NotUtf8)?;
        let crlf = text.matches("\r\n").count();
        let lf = text.matches('\n').count();
        let ending = match crlf {
            0 => LineEnding::Lf,
            n if n == lf => LineEnding::CrLf,
            _ => return Err(Unreadable::MixedLineEndings),
        };
        let sep = ending.as_str();
        let final_newline = text.ends_with(sep);
        let body = text.strip_suffix(sep).unwrap_or(text);
        let lines: Vec<String> = body.split(sep).map(str::to_owned).collect();
        let indent = detect_indent(&lines);
        Ok(Self {
            lines,
            ending,
            final_newline,
            indent,
            cursor: Pos::default(),
            anchor: None,
            goal: None,
            undo: Vec::new(),
            redo: Vec::new(),
            next_id: 1,
            saved: 0,
            typing: false,
            splices: Vec::new(),
        })
    }

    /// The file's bytes as a save writes them: the original separator and final newline.
    pub fn text(&self) -> String {
        let sep = self.ending.as_str();
        let mut out = self.lines.join(sep);
        if self.final_newline {
            out.push_str(sep);
        }
        out
    }

    pub fn lines(&self) -> &[String] {
        &self.lines
    }

    pub fn line_count(&self) -> usize {
        self.lines.len()
    }

    pub fn cursor(&self) -> Pos {
        self.cursor
    }

    pub fn ending(&self) -> LineEnding {
        self.ending
    }

    pub fn indent(&self) -> Indent {
        self.indent
    }

    /// The selected range, start before end, or `None` when nothing is selected.
    pub fn selection(&self) -> Option<(Pos, Pos)> {
        let anchor = self.anchor?;
        match anchor.cmp(&self.cursor) {
            std::cmp::Ordering::Less => Some((anchor, self.cursor)),
            std::cmp::Ordering::Greater => Some((self.cursor, anchor)),
            std::cmp::Ordering::Equal => None,
        }
    }

    /// Whether the text differs from the state last read or saved. Undoing back to that state
    /// makes the buffer clean again.
    pub fn is_dirty(&self) -> bool {
        self.state_id() != self.saved
    }

    /// Record that the current text is what is on disk now.
    pub fn mark_saved(&mut self) {
        self.saved = self.state_id();
        // The saved state must stay nameable: a typed character joining the saved step would
        // change the text under the id a later undo compares against.
        self.typing = false;
    }

    /// The line splices made since the last call, oldest first.
    pub fn take_splices(&mut self) -> Vec<Splice> {
        std::mem::take(&mut self.splices)
    }

    fn state_id(&self) -> u64 {
        self.undo.last().map_or(0, |s| s.id)
    }

    // ---- Movement -------------------------------------------------------------------------

    /// Put the cursor at `line` (clamped) and its first column, the place the editor opens at.
    pub fn place_at_line(&mut self, line: usize) {
        let line = line.min(self.lines.len() - 1);
        self.move_to(Pos::new(line, 0), false);
    }

    pub fn left(&mut self, word: bool, extend: bool) {
        if !extend && let Some((start, _)) = self.selection() {
            self.move_to(start, false);
            return;
        }
        let to = if word { self.word_left(self.cursor) } else { self.char_left(self.cursor) };
        self.move_to(to, extend);
    }

    pub fn right(&mut self, word: bool, extend: bool) {
        if !extend && let Some((_, end)) = self.selection() {
            self.move_to(end, false);
            return;
        }
        let to = if word { self.word_right(self.cursor) } else { self.char_right(self.cursor) };
        self.move_to(to, extend);
    }

    /// Move `n` lines up, keeping the goal column. On the first line the cursor goes to its
    /// start, the way a text field's `↑` does.
    pub fn up(&mut self, n: usize, extend: bool) {
        if self.cursor.line == 0 {
            self.move_to(Pos::new(0, 0), extend);
            return;
        }
        self.vertical(self.cursor.line.saturating_sub(n), extend);
    }

    /// Move `n` lines down, keeping the goal column. On the last line the cursor goes to its end.
    pub fn down(&mut self, n: usize, extend: bool) {
        let last = self.lines.len() - 1;
        if self.cursor.line == last {
            self.move_to(Pos::new(last, self.line_len(last)), extend);
            return;
        }
        self.vertical((self.cursor.line + n).min(last), extend);
    }

    /// `home`: the line's first non-blank column, or its very start when already there.
    pub fn home(&mut self, extend: bool) {
        let line = self.cursor.line;
        let first = self.lines[line].chars().take_while(|c| c.is_whitespace()).count();
        let col = if self.cursor.col == first { 0 } else { first };
        self.move_to(Pos::new(line, col), extend);
    }

    pub fn end(&mut self, extend: bool) {
        let line = self.cursor.line;
        self.move_to(Pos::new(line, self.line_len(line)), extend);
    }

    pub fn doc_start(&mut self, extend: bool) {
        self.move_to(Pos::new(0, 0), extend);
    }

    pub fn doc_end(&mut self, extend: bool) {
        let last = self.lines.len() - 1;
        self.move_to(Pos::new(last, self.line_len(last)), extend);
    }

    pub fn select_all(&mut self) {
        self.move_to(Pos::new(0, 0), false);
        self.doc_end(true);
    }

    /// Put the cursor at a display column of `line` (both clamped), as a click does.
    pub fn click(&mut self, line: usize, display_col: usize, extend: bool) {
        let line = line.min(self.lines.len() - 1);
        let col = self.col_at_display(line, display_col);
        self.move_to(Pos::new(line, col), extend);
    }

    /// Select `start..end` (both clamped) with the cursor at `end`, as a find step does.
    pub fn select(&mut self, start: Pos, end: Pos) {
        self.move_to(start, false);
        self.move_to(end, true);
    }

    /// Select the run of one character class under a display column of `line`, as a double
    /// click does: a word, a run of punctuation, or a run of blanks. Past the line's end it is
    /// the last run; an empty line selects nothing and leaves the cursor at its start.
    pub fn select_word(&mut self, line: usize, display_col: usize) {
        let line = line.min(self.lines.len() - 1);
        let chars: Vec<char> = self.lines[line].chars().collect();
        if chars.is_empty() {
            self.move_to(Pos::new(line, 0), false);
            return;
        }
        let at = self.col_at_display(line, display_col).min(chars.len() - 1);
        let class = char_class(chars[at]);
        let start = (0..at).rev().take_while(|&i| char_class(chars[i]) == class).last();
        let end = (at..chars.len()).take_while(|&i| char_class(chars[i]) == class).last();
        let (start, end) = (start.unwrap_or(at), end.unwrap_or(at) + 1);
        self.select(Pos::new(line, start), Pos::new(line, end));
    }

    /// Select `line` (clamped) whole, as a triple click does: through its line break to the
    /// next line's start, or to its end on the last line, so a cut takes the line.
    pub fn select_line(&mut self, line: usize) {
        // Clamped like a click: a triple click below the last line takes the last line.
        let line = line.min(self.lines.len() - 1);
        let end = if line + 1 < self.lines.len() {
            Pos::new(line + 1, 0)
        } else {
            Pos::new(line, self.line_len(line))
        };
        self.select(Pos::new(line, 0), end);
    }

    fn vertical(&mut self, line: usize, extend: bool) {
        let goal = self.goal.unwrap_or_else(|| self.display_col(self.cursor));
        let col = self.col_at_display(line, goal);
        self.move_to(Pos::new(line, col), extend);
        self.goal = Some(goal);
    }

    fn move_to(&mut self, to: Pos, extend: bool) {
        if extend {
            self.anchor.get_or_insert(self.cursor);
        } else {
            self.anchor = None;
        }
        self.cursor = self.clamp(to);
        self.goal = None;
        self.typing = false;
    }

    fn char_left(&self, p: Pos) -> Pos {
        if p.col > 0 {
            Pos::new(p.line, p.col - 1)
        } else if p.line > 0 {
            Pos::new(p.line - 1, self.line_len(p.line - 1))
        } else {
            p
        }
    }

    fn char_right(&self, p: Pos) -> Pos {
        if p.col < self.line_len(p.line) {
            Pos::new(p.line, p.col + 1)
        } else if p.line + 1 < self.lines.len() {
            Pos::new(p.line + 1, 0)
        } else {
            p
        }
    }

    /// The start of the word before `p`: blanks are skipped, then one run of the same class.
    /// At a line's start it crosses to the previous line's end.
    fn word_left(&self, p: Pos) -> Pos {
        if p.col == 0 {
            return self.char_left(p);
        }
        let chars: Vec<char> = self.lines[p.line].chars().collect();
        let mut col = p.col;
        while col > 0 && chars[col - 1].is_whitespace() {
            col -= 1;
        }
        if col > 0 {
            let class = char_class(chars[col - 1]);
            while col > 0 && char_class(chars[col - 1]) == class {
                col -= 1;
            }
        }
        Pos::new(p.line, col)
    }

    /// The end of the word after `p`, mirroring [`Self::word_left`].
    fn word_right(&self, p: Pos) -> Pos {
        let chars: Vec<char> = self.lines[p.line].chars().collect();
        if p.col >= chars.len() {
            return self.char_right(p);
        }
        let mut col = p.col;
        while col < chars.len() && chars[col].is_whitespace() {
            col += 1;
        }
        if col < chars.len() {
            let class = char_class(chars[col]);
            while col < chars.len() && char_class(chars[col]) == class {
                col += 1;
            }
        }
        Pos::new(p.line, col)
    }

    // ---- Display columns ------------------------------------------------------------------

    /// The display column of `p`: tabs expanded to the next stop, wide characters counted twice.
    pub fn display_col(&self, p: Pos) -> usize {
        let mut width = 0;
        for ch in self.lines[p.line].chars().take(p.col) {
            width += char_display_width(ch, width);
        }
        width
    }

    /// The char column a display column falls in. A column inside a tab or a wide character
    /// lands before it; one past the end lands at the end.
    pub fn col_at_display(&self, line: usize, target: usize) -> usize {
        let mut width = 0;
        for (col, ch) in self.lines[line].chars().enumerate() {
            let w = char_display_width(ch, width);
            if width + w > target {
                return col;
            }
            width += w;
        }
        self.line_len(line)
    }

    // ---- Edits ----------------------------------------------------------------------------

    /// Type one character over the selection, or at the cursor. A run of typed characters is
    /// one undo step, broken where a blank follows a word.
    pub fn type_char(&mut self, ch: char) {
        let (start, end) = self.selection_or_cursor();
        let joins = start == end
            && self.typing
            && self.undo.last().is_some_and(|top| {
                let ends_word = ch.is_whitespace() && !top.inserted.ends_with(char::is_whitespace);
                top.id != self.saved && top.cursor_after == start && !ends_word
            });
        let mut buf = [0u8; 4];
        let text: &str = ch.encode_utf8(&mut buf);
        if joins {
            let (_, after) = self.replace(start, end, text);
            let top = self.undo.last_mut().expect("a joined step exists");
            top.inserted.push_str(text);
            top.cursor_after = after;
            self.redo.clear();
            self.cursor = after;
            self.anchor = None;
            self.goal = None;
        } else {
            self.apply(start, end, text);
        }
        self.typing = true;
    }

    /// Insert text (a paste) over the selection, or at the cursor. Its line breaks become the
    /// buffer's own, so a pasted `\r\n` never reaches a line.
    pub fn insert(&mut self, text: &str) {
        let text = text.replace("\r\n", "\n").replace('\r', "\n");
        let (start, end) = self.selection_or_cursor();
        self.apply(start, end, &text);
    }

    /// `enter`: break the line, carrying the current line's indentation onto the new one.
    pub fn newline(&mut self) {
        let (start, end) = self.selection_or_cursor();
        let indent: String = self.lines[start.line]
            .chars()
            .take(start.col)
            .take_while(|c| *c == ' ' || *c == '\t')
            .collect();
        self.apply(start, end, &format!("\n{indent}"));
    }

    /// `tab`: indent every line a multi-line selection touches, or insert one indent unit.
    pub fn tab(&mut self) {
        if let Some((start, end)) = self.selection()
            && start.line != end.line
        {
            self.reindent(start.line, end.line, true);
            return;
        }
        let unit = match self.indent {
            Indent::Tab => "\t".to_owned(),
            Indent::Spaces(n) => {
                let (start, _) = self.selection_or_cursor();
                " ".repeat(n - self.display_col(start) % n)
            }
        };
        let (start, end) = self.selection_or_cursor();
        self.apply(start, end, &unit);
    }

    /// `shift+tab`: take one indent unit off every line the selection (or the cursor) touches.
    pub fn backtab(&mut self) {
        let (start, end) = self.selection_or_cursor();
        self.reindent(start.line, end.line, false);
    }

    pub fn backspace(&mut self) {
        if self.delete_selection() {
            return;
        }
        let from = self.char_left(self.cursor);
        if from != self.cursor {
            self.apply(from, self.cursor, "");
        }
    }

    pub fn delete(&mut self) {
        if self.delete_selection() {
            return;
        }
        let to = self.char_right(self.cursor);
        if to != self.cursor {
            self.apply(self.cursor, to, "");
        }
    }

    /// `alt+backspace`: delete back to the start of the word.
    pub fn delete_word_back(&mut self) {
        if self.delete_selection() {
            return;
        }
        let from = self.word_left(self.cursor);
        if from != self.cursor {
            self.apply(from, self.cursor, "");
        }
    }

    /// The text `copy` takes: the selection, or the cursor's whole line with its break.
    pub fn copy_text(&self) -> String {
        match self.selection() {
            Some((start, end)) => self.slice(start, end),
            None => format!("{}\n", self.lines[self.cursor.line]),
        }
    }

    /// `cut`: remove and return the selection, or the cursor's whole line.
    pub fn cut(&mut self) -> String {
        match self.selection() {
            Some((start, end)) => {
                let text = self.slice(start, end);
                self.apply(start, end, "");
                text
            }
            None => self.cut_line(),
        }
    }

    /// Remove the lines the selection (or the cursor) touches, returning them with a break
    /// after each.
    pub fn cut_line(&mut self) -> String {
        let (start, end) = self.selection_or_cursor();
        let (first, last) = (start.line, end.line);
        let mut text = self.lines[first..=last].join("\n");
        text.push('\n');
        let (from, to) = if last + 1 < self.lines.len() {
            (Pos::new(first, 0), Pos::new(last + 1, 0))
        } else if first > 0 {
            (Pos::new(first - 1, self.line_len(first - 1)), Pos::new(last, self.line_len(last)))
        } else {
            (Pos::new(0, 0), Pos::new(last, self.line_len(last)))
        };
        self.apply(from, to, "");
        // The cursor lands at the start of what followed, not mid-line on the line above.
        let line = self.cursor.line.min(self.lines.len() - 1);
        self.cursor = Pos::new(if from.line < first { line + 1 } else { line }, 0);
        self.cursor = self.clamp(self.cursor);
        text
    }

    /// `ctrl+d`: duplicate the selection after itself, or the cursor's line below it.
    pub fn duplicate(&mut self) {
        if let Some((start, end)) = self.selection() {
            let text = self.slice(start, end);
            self.apply(end, end, &text);
            // Keep the copy selected, so a second press duplicates again.
            self.anchor = Some(end);
        } else {
            let line = self.cursor.line;
            let col = self.cursor.col;
            let text = format!("\n{}", self.lines[line]);
            let at = Pos::new(line, self.line_len(line));
            self.apply(at, at, &text);
            self.cursor = Pos::new(line + 1, col);
        }
    }

    /// Take back the last step. Returns whether there was one.
    pub fn undo(&mut self) -> bool {
        let Some(step) = self.undo.pop() else { return false };
        let end = end_of(step.start, &step.inserted);
        self.replace(step.start, end, &step.removed);
        self.cursor = step.cursor_before;
        self.anchor = step.anchor_before;
        self.goal = None;
        self.typing = false;
        self.redo.push(step);
        true
    }

    /// Replay the last undone step. Returns whether there was one.
    pub fn redo(&mut self) -> bool {
        let Some(step) = self.redo.pop() else { return false };
        let end = end_of(step.start, &step.removed);
        self.replace(step.start, end, &step.inserted);
        self.cursor = step.cursor_after;
        self.anchor = None;
        self.goal = None;
        self.typing = false;
        self.undo.push(step);
        true
    }

    fn delete_selection(&mut self) -> bool {
        let Some((start, end)) = self.selection() else { return false };
        self.apply(start, end, "");
        true
    }

    fn selection_or_cursor(&self) -> (Pos, Pos) {
        self.selection().unwrap_or((self.cursor, self.cursor))
    }

    /// Indent or dedent lines `first..=last` as one undo step, leaving them selected.
    fn reindent(&mut self, first: usize, last: usize, add: bool) {
        let unit = match self.indent {
            Indent::Tab => "\t".to_owned(),
            Indent::Spaces(n) => " ".repeat(n),
        };
        let changed: Vec<String> = self.lines[first..=last]
            .iter()
            .map(|line| {
                if add {
                    if line.is_empty() { String::new() } else { format!("{unit}{line}") }
                } else {
                    let strip = match self.indent {
                        Indent::Tab if line.starts_with('\t') => 1,
                        Indent::Tab => 0,
                        Indent::Spaces(n) => line.chars().take(n).take_while(|c| *c == ' ').count(),
                    };
                    line.chars().skip(strip).collect()
                }
            })
            .collect();
        if changed.as_slice() == &self.lines[first..=last] {
            return;
        }
        let multi = first != last;
        let cursor_col = self.cursor.col;
        let old_len = self.line_len(self.cursor.line);
        let from = Pos::new(first, 0);
        let to = Pos::new(last, self.line_len(last));
        self.apply(from, to, &changed.join("\n"));
        if multi {
            self.anchor = Some(Pos::new(first, 0));
            self.cursor = Pos::new(last, self.line_len(last));
        } else {
            // One line keeps the cursor on the same character it was on.
            let line = first;
            let new_len = self.line_len(line);
            let col = (cursor_col + new_len).saturating_sub(old_len);
            self.cursor = Pos::new(line, col.min(new_len));
        }
    }

    /// Replace `start..end` with `text` as one new undo step, leaving the cursor after it.
    fn apply(&mut self, start: Pos, end: Pos, text: &str) {
        let cursor_before = self.cursor;
        let anchor_before = self.anchor;
        let (removed, after) = self.replace(start, end, text);
        let id = self.next_id;
        self.next_id += 1;
        self.undo.push(Step {
            id,
            start,
            removed,
            inserted: text.to_owned(),
            cursor_before,
            anchor_before,
            cursor_after: after,
        });
        self.redo.clear();
        self.cursor = after;
        self.anchor = None;
        self.goal = None;
        self.typing = false;
    }

    /// The one mutation: replace `start..end` with `text`, returning the text removed and the
    /// position just after the inserted text. Records the line splice.
    fn replace(&mut self, start: Pos, end: Pos, text: &str) -> (String, Pos) {
        debug_assert!(start <= end, "a replaced range runs forward");
        let removed = self.slice(start, end);
        let head = {
            let line = &self.lines[start.line];
            line[..byte_of(line, start.col)].to_owned()
        };
        let tail = {
            let line = &self.lines[end.line];
            line[byte_of(line, end.col)..].to_owned()
        };
        let mut new: Vec<String> = text.split('\n').map(str::to_owned).collect();
        let last_len = new.last().map_or(0, |l| l.chars().count());
        let after = if new.len() == 1 {
            Pos::new(start.line, start.col + last_len)
        } else {
            Pos::new(start.line + new.len() - 1, last_len)
        };
        new[0].insert_str(0, &head);
        new.last_mut().expect("split yields one piece at least").push_str(&tail);
        let inserted = new.len();
        self.lines.splice(start.line..=end.line, new);
        self.splices.push(Splice {
            start: start.line,
            removed: end.line - start.line + 1,
            inserted,
        });
        (removed, after)
    }

    /// The text between two positions, lines joined with `\n`.
    fn slice(&self, start: Pos, end: Pos) -> String {
        if start.line == end.line {
            return self.lines[start.line]
                .chars()
                .skip(start.col)
                .take(end.col - start.col)
                .collect();
        }
        let mut out: String = self.lines[start.line].chars().skip(start.col).collect();
        for line in &self.lines[start.line + 1..end.line] {
            out.push('\n');
            out.push_str(line);
        }
        out.push('\n');
        out.extend(self.lines[end.line].chars().take(end.col));
        out
    }

    fn clamp(&self, p: Pos) -> Pos {
        let line = p.line.min(self.lines.len() - 1);
        Pos::new(line, p.col.min(self.line_len(line)))
    }

    fn line_len(&self, line: usize) -> usize {
        self.lines[line].chars().count()
    }
}

/// The position just past `text` inserted at `start`.
fn end_of(start: Pos, text: &str) -> Pos {
    match text.rfind('\n') {
        None => Pos::new(start.line, start.col + text.chars().count()),
        Some(i) => Pos::new(start.line + text.matches('\n').count(), text[i + 1..].chars().count()),
    }
}

/// The byte offset of char column `col` in `line`, or its length past the end.
fn byte_of(line: &str, col: usize) -> usize {
    line.char_indices().nth(col).map_or(line.len(), |(b, _)| b)
}

/// The width `ch` takes when it starts at display column `at`: a tab runs to the next stop, a
/// control character paints as one replacement cell, everything else is its Unicode width.
pub fn char_display_width(ch: char, at: usize) -> usize {
    if ch == '\t' { TAB_WIDTH - at % TAB_WIDTH } else { UnicodeWidthChar::width(ch).unwrap_or(1) }
}

/// The three classes a word move tells apart.
#[derive(PartialEq, Eq)]
enum CharClass {
    Blank,
    Word,
    Punct,
}

fn char_class(ch: char) -> CharClass {
    if ch.is_whitespace() {
        CharClass::Blank
    } else if ch.is_alphanumeric() || ch == '_' {
        CharClass::Word
    } else {
        CharClass::Punct
    }
}

/// The indent unit a file already uses. Tabs win when more lines start with one than with two
/// or more spaces. Otherwise the unit is the most common step by which one line's leading
/// spaces exceed the previous non-blank line's, between 2 and 8. A file that says nothing
/// either way gets four spaces.
fn detect_indent(lines: &[String]) -> Indent {
    let tabbed = lines.iter().filter(|l| l.starts_with('\t')).count();
    let spaced = lines.iter().filter(|l| l.starts_with("  ")).count();
    if tabbed > spaced {
        return Indent::Tab;
    }
    let mut steps = [0usize; 9];
    let mut prev = 0;
    for line in lines.iter().filter(|l| !l.trim().is_empty()) {
        let lead = line.chars().take_while(|c| *c == ' ').count();
        if lead > prev && (2..=8).contains(&(lead - prev)) {
            steps[lead - prev] += 1;
        }
        prev = lead;
    }
    let best = (2..=8).max_by_key(|&n| (steps[n], n == 4)).unwrap_or(4);
    Indent::Spaces(if steps[best] == 0 { 4 } else { best })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn buf(text: &str) -> Buffer {
        Buffer::parse(text.as_bytes()).unwrap()
    }

    /// The selected text, `None` with nothing selected.
    fn selected_text(b: &Buffer) -> Option<String> {
        b.selection().map(|_| b.copy_text())
    }

    #[test]
    fn a_word_selection_takes_one_run_of_a_character_class() {
        let mut b = buf("let foo_1 = a.b;   x\n\n");
        b.select_word(0, 5);
        assert_eq!(selected_text(&b).as_deref(), Some("foo_1"));
        assert_eq!(b.cursor(), Pos::new(0, 9), "the cursor ends the word");
        b.select_word(0, 15);
        assert_eq!(selected_text(&b).as_deref(), Some(";"), "punctuation is its own run");
        b.select_word(0, 17);
        assert_eq!(selected_text(&b).as_deref(), Some("   "), "and so are blanks");
        b.select_word(0, 99);
        assert_eq!(selected_text(&b).as_deref(), Some("x"), "past the end is the last run");
        b.select_word(1, 3);
        assert_eq!(b.selection(), None, "an empty line has no word");
        assert_eq!(b.cursor(), Pos::new(1, 0));
    }

    #[test]
    fn a_word_selection_counts_display_columns_past_a_tab() {
        let mut b = buf("\tab cd\n");
        b.select_word(0, 4); // the tab spans columns 0..4, so 4 is `a`
        assert_eq!(selected_text(&b).as_deref(), Some("ab"));
    }

    #[test]
    fn a_line_selection_takes_the_break_except_on_the_last_line() {
        let mut b = buf("one\ntwo");
        b.select_line(0);
        assert_eq!(b.selection(), Some((Pos::new(0, 0), Pos::new(1, 0))));
        b.select_line(9);
        assert_eq!(b.selection(), Some((Pos::new(1, 0), Pos::new(1, 3))), "clamped to the last");
    }

    /// The buffer's text with `|` at the cursor, for compact assertions.
    fn shown(b: &Buffer) -> String {
        let mut lines = b.lines().to_vec();
        let c = b.cursor();
        let at = byte_of(&lines[c.line], c.col);
        lines[c.line].insert(at, '|');
        lines.join("\n")
    }

    #[test]
    fn a_round_trip_keeps_the_separator_and_the_final_newline() {
        for text in
            ["", "a", "a\n", "a\nb", "a\nb\n", "\n", "a\n\n", "a\r\nb\r\n", "a\r\nb", "\r\n"]
        {
            assert_eq!(buf(text).text(), text, "{text:?} survives a read and a write");
        }
    }

    #[test]
    fn an_empty_file_is_one_empty_line() {
        let b = buf("");
        assert_eq!(b.lines(), [""]);
        assert_eq!(b.line_count(), 1);
    }

    #[test]
    fn crlf_lines_hold_no_carriage_return() {
        let b = buf("one\r\ntwo\r\n");
        assert_eq!(b.ending(), LineEnding::CrLf);
        assert_eq!(b.lines(), ["one", "two"]);
    }

    #[test]
    fn what_a_save_would_corrupt_is_refused() {
        assert_eq!(Buffer::parse(b"a\r\nb\nc").unwrap_err(), Unreadable::MixedLineEndings);
        assert_eq!(Buffer::parse(b"a\0b").unwrap_err(), Unreadable::Binary);
        assert_eq!(Buffer::parse(&[0xff, 0xfe, b'a']).unwrap_err(), Unreadable::NotUtf8);
    }

    #[test]
    fn typing_inserts_at_the_cursor_and_moves_past_it() {
        let mut b = buf("ac\n");
        b.right(false, false);
        b.type_char('b');
        assert_eq!(shown(&b), "ab|c");
        assert_eq!(b.text(), "abc\n");
        assert!(b.is_dirty());
    }

    #[test]
    fn multibyte_text_edits_by_character() {
        let mut b = buf("привет");
        b.end(false);
        b.backspace();
        b.type_char('ы');
        assert_eq!(shown(&b), "привеы|");
    }

    #[test]
    fn enter_splits_the_line_and_carries_its_indent() {
        let mut b = buf("    let x = 1;");
        b.end(false);
        b.left(false, false);
        b.newline();
        assert_eq!(shown(&b), "    let x = 1\n    |;");
    }

    #[test]
    fn backspace_and_delete_join_lines_at_their_edges() {
        let mut b = buf("ab\ncd");
        b.down(1, false);
        b.home(false);
        b.home(false);
        b.backspace();
        assert_eq!(shown(&b), "ab|cd");
        b.delete();
        assert_eq!(shown(&b), "ab|d");
        b.doc_start(false);
        b.backspace();
        assert_eq!(shown(&b), "|abd", "backspace at the very start does nothing");
        b.doc_end(false);
        b.delete();
        assert_eq!(shown(&b), "abd|", "delete at the very end does nothing");
    }

    #[test]
    fn typing_over_a_selection_replaces_it() {
        let mut b = buf("hello world");
        b.right(true, false);
        b.right(true, true);
        b.right(true, true);
        assert_eq!(b.selection(), Some((Pos::new(0, 5), Pos::new(0, 11))));
        b.type_char('!');
        assert_eq!(shown(&b), "hello!|");
    }

    #[test]
    fn a_plain_arrow_collapses_the_selection_to_its_edge() {
        let mut b = buf("abcdef");
        b.right(false, false);
        b.right(false, true);
        b.right(false, true);
        b.left(false, false);
        assert_eq!(shown(&b), "a|bcdef");
        assert_eq!(b.selection(), None);
        b.right(false, true);
        b.right(false, false);
        assert_eq!(shown(&b), "ab|cdef");
    }

    #[test]
    fn vertical_moves_keep_the_goal_column_across_a_short_line() {
        let mut b = buf("abcdef\nab\nabcdef");
        b.end(false);
        b.down(1, false);
        assert_eq!(b.cursor(), Pos::new(1, 2));
        b.down(1, false);
        assert_eq!(b.cursor(), Pos::new(2, 6), "the goal survives the short line");
        b.down(1, false);
        assert_eq!(b.cursor(), Pos::new(2, 6), "the last line's end is the floor");
        b.up(5, false);
        assert_eq!(b.cursor(), Pos::new(0, 6));
        b.up(1, false);
        assert_eq!(b.cursor(), Pos::new(0, 0), "the first line's start is the ceiling");
    }

    #[test]
    fn vertical_moves_aim_by_display_column_through_tabs() {
        let mut b = buf("\tx\nabcdef");
        b.end(false);
        assert_eq!(b.display_col(b.cursor()), 5);
        b.down(1, false);
        assert_eq!(b.cursor(), Pos::new(1, 5));
        b.up(1, false);
        assert_eq!(b.cursor(), Pos::new(0, 2));
    }

    #[test]
    fn a_click_inside_a_tab_or_wide_character_lands_before_it() {
        let b = buf("\tx\n日本");
        assert_eq!(b.col_at_display(0, 2), 0);
        assert_eq!(b.col_at_display(0, 4), 1);
        assert_eq!(b.col_at_display(1, 1), 0);
        assert_eq!(b.col_at_display(1, 2), 1);
        assert_eq!(b.col_at_display(1, 99), 2, "past the end lands at the end");
    }

    #[test]
    fn home_toggles_between_the_first_non_blank_and_the_line_start() {
        let mut b = buf("    code");
        b.end(false);
        b.home(false);
        assert_eq!(b.cursor().col, 4);
        b.home(false);
        assert_eq!(b.cursor().col, 0);
        b.home(false);
        assert_eq!(b.cursor().col, 4);
    }

    #[test]
    fn word_moves_skip_blanks_then_one_class_and_cross_lines() {
        let mut b = buf("foo.bar  baz\nnext");
        b.right(true, false);
        assert_eq!(b.cursor().col, 3);
        b.right(true, false);
        assert_eq!(b.cursor().col, 4, "punctuation is a word of its own");
        b.right(true, false);
        assert_eq!(b.cursor().col, 7);
        b.right(true, false);
        assert_eq!(b.cursor().col, 12);
        b.right(true, false);
        assert_eq!(b.cursor(), Pos::new(1, 0));
        b.left(true, false);
        assert_eq!(b.cursor(), Pos::new(0, 12));
        b.left(true, false);
        assert_eq!(b.cursor().col, 9);
    }

    #[test]
    fn undo_takes_back_a_typed_word_and_redo_replays_it() {
        let mut b = buf("");
        for ch in "one two".chars() {
            b.type_char(ch);
        }
        assert!(b.undo());
        assert_eq!(shown(&b), "one|", "the word after the blank is one step");
        assert!(b.undo());
        assert_eq!(shown(&b), "|");
        assert!(!b.is_dirty(), "undone to the read state, the buffer is clean");
        assert!(!b.undo());
        assert!(b.redo());
        assert!(b.redo());
        assert_eq!(shown(&b), "one two|");
        assert!(!b.redo());
    }

    #[test]
    fn a_new_edit_after_undo_drops_the_redo_branch() {
        let mut b = buf("x");
        b.end(false);
        b.type_char('a');
        b.undo();
        b.type_char('b');
        assert!(!b.redo());
        assert_eq!(b.text(), "xb");
    }

    #[test]
    fn undo_restores_the_selection_an_edit_replaced() {
        let mut b = buf("abc");
        b.select_all();
        b.type_char('z');
        b.undo();
        assert_eq!(b.text(), "abc");
        assert_eq!(b.selection(), Some((Pos::new(0, 0), Pos::new(0, 3))));
    }

    #[test]
    fn undo_and_redo_cross_line_breaks() {
        let mut b = buf("ab\ncd\nef");
        b.right(false, false);
        b.down(1, true);
        b.down(1, true);
        b.insert("X\nY");
        assert_eq!(b.text(), "aX\nYf");
        b.undo();
        assert_eq!(b.text(), "ab\ncd\nef");
        b.redo();
        assert_eq!(b.text(), "aX\nYf");
    }

    #[test]
    fn saving_makes_the_buffer_clean_and_typing_after_it_dirty_again() {
        let mut b = buf("");
        b.type_char('a');
        b.mark_saved();
        assert!(!b.is_dirty());
        b.type_char('b');
        assert!(b.is_dirty(), "a character typed after a save does not join the saved step");
        b.undo();
        assert!(!b.is_dirty(), "undoing back to the saved text is clean");
        b.undo();
        assert!(b.is_dirty(), "and undoing past it is not");
    }

    #[test]
    fn a_paste_normalizes_line_breaks() {
        let mut b = buf("");
        b.insert("a\r\nb\rc");
        assert_eq!(b.lines(), ["a", "b", "c"]);
        assert_eq!(shown(&b), "a\nb\nc|");
    }

    #[test]
    fn tab_inserts_the_files_unit_up_to_the_next_stop() {
        let mut b = buf("fn x() {\n  y\n}");
        assert_eq!(b.indent(), Indent::Spaces(2));
        b.right(false, false);
        b.tab();
        assert_eq!(shown(&b).lines().next().unwrap(), "f |n x() {");
        let mut t = buf("a\n\tb\n\tc");
        assert_eq!(t.indent(), Indent::Tab);
        t.tab();
        assert_eq!(t.lines()[0], "\ta");
    }

    #[test]
    fn tab_and_backtab_reindent_a_multi_line_selection_as_one_step() {
        let mut b = buf("a\n\nb");
        b.select_all();
        b.tab();
        assert_eq!(b.lines(), ["    a", "", "    b"], "blank lines stay blank");
        b.backtab();
        assert_eq!(b.lines(), ["a", "", "b"]);
        b.undo();
        assert_eq!(b.lines(), ["    a", "", "    b"]);
    }

    #[test]
    fn backtab_on_one_line_keeps_the_cursor_on_its_character() {
        let mut b = buf("a\n    b\n      x");
        assert_eq!(b.indent(), Indent::Spaces(4));
        b.doc_end(false);
        let last = |b: &Buffer| shown(b).lines().last().unwrap().to_owned();
        b.backtab();
        assert_eq!(last(&b), "  x|");
        b.backtab();
        assert_eq!(last(&b), "x|");
        b.backtab();
        assert_eq!(last(&b), "x|", "nothing left to take off");
        assert!(b.undo());
        assert_eq!(last(&b), "  x|");
    }

    #[test]
    fn indent_detection_reads_the_files_own_step() {
        let lines = |t: &str| t.lines().map(str::to_owned).collect::<Vec<_>>();
        assert_eq!(detect_indent(&lines("a\n    b\n        c\n    d")), Indent::Spaces(4));
        assert_eq!(detect_indent(&lines("a:\n  b:\n    c")), Indent::Spaces(2));
        assert_eq!(detect_indent(&lines("a\n\tb")), Indent::Tab);
        assert_eq!(detect_indent(&lines("flat\ntext")), Indent::Spaces(4));
        assert_eq!(
            detect_indent(&lines("/**\n * doc\n */")),
            Indent::Spaces(4),
            "a one-space comment gutter is not an indent"
        );
    }

    #[test]
    fn copy_and_cut_take_the_selection_or_the_whole_line() {
        let mut b = buf("one\ntwo\nthree");
        b.down(1, false);
        assert_eq!(b.copy_text(), "two\n");
        assert_eq!(b.cut(), "two\n");
        assert_eq!(b.lines(), ["one", "three"]);
        assert_eq!(b.cursor(), Pos::new(1, 0));
        b.right(true, true);
        assert_eq!(b.cut(), "three");
        assert_eq!(b.lines(), ["one", ""]);
    }

    #[test]
    fn cutting_the_last_line_takes_the_break_before_it() {
        let mut b = buf("one\ntwo");
        b.down(1, false);
        assert_eq!(b.cut_line(), "two\n");
        assert_eq!(b.lines(), ["one"]);
        assert_eq!(b.cursor(), Pos::new(0, 0));
        assert_eq!(b.cut_line(), "one\n");
        assert_eq!(b.lines(), [""], "cutting the only line leaves one empty line");
    }

    #[test]
    fn duplicate_copies_the_line_below_or_the_selection_after_itself() {
        let mut b = buf("ab\ncd");
        b.right(false, false);
        b.duplicate();
        assert_eq!(b.lines(), ["ab", "ab", "cd"]);
        assert_eq!(b.cursor(), Pos::new(1, 1));
        let mut s = buf("xy");
        s.right(false, true);
        s.duplicate();
        assert_eq!(s.text(), "xxy");
        assert_eq!(s.selection(), Some((Pos::new(0, 1), Pos::new(0, 2))));
    }

    #[test]
    fn delete_word_back_stops_at_the_word_start() {
        let mut b = buf("let value = 1");
        b.right(true, false);
        b.right(true, false);
        b.delete_word_back();
        assert_eq!(shown(&b), "let | = 1");
    }

    #[test]
    fn every_edit_reports_its_line_splice() {
        let mut b = buf("a\nb\nc");
        b.take_splices();
        b.down(1, false);
        b.end(false);
        b.newline();
        assert_eq!(b.take_splices(), [Splice { start: 1, removed: 1, inserted: 2 }]);
        b.select_all();
        b.type_char('z');
        assert_eq!(b.take_splices(), [Splice { start: 0, removed: 4, inserted: 1 }]);
        b.undo();
        assert_eq!(b.take_splices(), [Splice { start: 0, removed: 1, inserted: 4 }]);
        assert!(b.take_splices().is_empty());
    }

    #[test]
    fn place_at_line_clamps_past_the_end() {
        let mut b = buf("a\nb");
        b.place_at_line(9);
        assert_eq!(b.cursor(), Pos::new(1, 0));
    }
}
