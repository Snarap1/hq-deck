//! SCM mode state, the `Changes` navigator's second face: the Staged/Changes sections, commit
//! box, the seven read-only drawers, and the context menu — everything the SCM navigator needs
//! beyond a `git status`.
//!
//! Built on this crate's `git::porcelain`/`git::drawers`. This module is pure state and
//! git-calling logic; rendering and mouse hit-testing live in `ui.rs`, and `App` holds exactly
//! one field (`scm: ScmPanel`) plus delegating methods - don't bloat `app.rs` with SCM state.
//!
//! Every git call here runs synchronously between frames, like the rest of this crate's
//! `reload()` — `status`/drawer reads are single small subprocesses, the same cost class as the
//! scope-diff build `App::reload` already does inline (draft §3.8, plan M2 step 11).

use std::cell::Cell;
use std::path::Path;

use anyhow::Result;

use crate::git::drawers;
use crate::git::porcelain::{self, FileEntry, Status};

/// One of the seven read-only drawers below the Staged/Changes sections.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DrawerKind {
    Graph,
    Commits,
    Branches,
    Remotes,
    Stashes,
    Tags,
    Worktrees,
}

impl DrawerKind {
    pub const ALL: [DrawerKind; 7] = [
        DrawerKind::Graph,
        DrawerKind::Commits,
        DrawerKind::Branches,
        DrawerKind::Remotes,
        DrawerKind::Stashes,
        DrawerKind::Tags,
        DrawerKind::Worktrees,
    ];

    pub fn label(self) -> &'static str {
        match self {
            DrawerKind::Graph => "GRAPH",
            DrawerKind::Commits => "COMMITS",
            DrawerKind::Branches => "BRANCHES",
            DrawerKind::Remotes => "REMOTES",
            DrawerKind::Stashes => "STASHES",
            DrawerKind::Tags => "TAGS",
            DrawerKind::Worktrees => "WORKTREES",
        }
    }

    fn index(self) -> usize {
        DrawerKind::ALL.iter().position(|k| *k == self).expect("every kind listed")
    }

    /// The drawer's lines: a plain `git` list, newest/most-relevant first — never a mutation.
    fn fetch(self, repo: &Path) -> Result<Vec<String>> {
        const LIMIT: usize = 100;
        match self {
            DrawerKind::Graph => drawers::graph(repo, LIMIT),
            DrawerKind::Commits => drawers::commits(repo, LIMIT),
            DrawerKind::Branches => drawers::branches(repo),
            DrawerKind::Remotes => drawers::remotes(repo),
            DrawerKind::Stashes => drawers::stashes(repo),
            DrawerKind::Tags => drawers::tags(repo),
            DrawerKind::Worktrees => drawers::worktrees(repo),
        }
    }
}

/// One drawer's expand state and last-fetched lines.
#[derive(Clone, Debug, Default)]
pub struct Drawer {
    pub expanded: bool,
    pub lines: Vec<String>,
}

/// One flattened row of the SCM navigator, in paint order: the Staged section, the Changes
/// section, then the seven drawers.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Row {
    StagedHeader,
    Staged(usize),
    ChangesHeader,
    Unstaged(usize),
    DrawerHeader(DrawerKind),
    DrawerLine(DrawerKind, usize),
}

/// Which pane has the keyboard while SCM mode is active.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ScmFocus {
    #[default]
    List,
    /// Typing the commit message — routed through `Mode::ScmMessage` (`app.rs`), which shares
    /// this crate's one caret-editing control set (`App::edit_input`).
    Message,
}

/// What a context-menu entry or a pending confirm acts on.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum MenuTarget {
    Staged(usize),
    Unstaged(usize),
    Drawer(DrawerKind, usize),
}

/// One menu action. Each is either run immediately ([`ScmPanel::run_menu_action`]) or, for a
/// destructive one, staged as a [`PendingConfirm`] first.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MenuAction {
    Stage,
    Unstage,
    Discard,
    CopyPath,
    CopyRelativePath,
    Checkout,
    /// `merge --no-edit <branch>` — stays a menu-only action here, never duplicated as a
    /// top-level command (plan M2 step 6, M6 draft note).
    MergeInto,
    DeleteBranch,
    ApplyStash,
    PopStash,
    DropStash,
    CheckoutTag,
    DeleteTag,
    CopyName,
    FetchRemote,
    RemoveWorktree,
}

impl MenuAction {
    /// Whether this action mutates the repository (as opposed to a clipboard copy or a
    /// checkout-only navigation) and so needs the reviewer to confirm it first.
    fn destructive(self) -> bool {
        matches!(
            self,
            MenuAction::Discard
                | MenuAction::DeleteBranch
                | MenuAction::DropStash
                | MenuAction::DeleteTag
                | MenuAction::RemoveWorktree
        )
    }

    fn label(self) -> &'static str {
        match self {
            MenuAction::Stage => "Stage",
            MenuAction::Unstage => "Unstage",
            MenuAction::Discard => "Discard changes…",
            MenuAction::CopyPath => "Copy path",
            MenuAction::CopyRelativePath => "Copy relative path",
            MenuAction::Checkout => "Checkout",
            MenuAction::MergeInto => "Merge into current branch",
            MenuAction::DeleteBranch => "Delete branch…",
            MenuAction::ApplyStash => "Apply stash",
            MenuAction::PopStash => "Pop stash",
            MenuAction::DropStash => "Drop stash…",
            MenuAction::CheckoutTag => "Checkout tag",
            MenuAction::DeleteTag => "Delete tag…",
            MenuAction::CopyName => "Copy name",
            MenuAction::FetchRemote => "Fetch",
            MenuAction::RemoveWorktree => "Remove worktree…",
        }
    }
}

#[derive(Clone, Debug)]
pub struct MenuEntry {
    pub action: MenuAction,
    pub label: &'static str,
}

#[derive(Clone, Debug)]
pub struct ContextMenu {
    pub target: MenuTarget,
    pub entries: Vec<MenuEntry>,
    pub selected: usize,
}

#[derive(Clone, Debug)]
pub struct PendingConfirm {
    pub message: String,
    pub target: MenuTarget,
    pub action: MenuAction,
}

/// The stable identity a row reconciles by across a refresh (Continuity: identity first, then
/// nearest surviving, then clamp — never row index).
#[derive(Clone, Debug, PartialEq, Eq)]
enum Identity {
    Staged(String),
    Unstaged(String),
    DrawerHeader(DrawerKind),
    DrawerLine(DrawerKind, String),
}

#[derive(Debug, Default)]
pub struct ScmPanel {
    pub status: Status,
    rows: Vec<Row>,
    pub cursor: usize,
    /// Top visible row. Independent of `cursor`, like the file list's `file_scroll`: a cursor
    /// move asks for a reveal ([`Self::reveal`]), the wheel moves this alone, and every frame
    /// clamps it so an over-scroll never shows a blank tail ([`Self::reveal_scroll`]).
    scroll: Cell<usize>,
    /// Whether the next frame must pull `cursor` back into view. Set by the user's own cursor
    /// moves and cleared by the frame that honors it — never set by a refresh, so a poll or a
    /// stage cannot yank a wheeled viewport back (Continuity).
    reveal: Cell<bool>,
    pub staged_collapsed: bool,
    pub changes_collapsed: bool,
    drawers: [Drawer; 7],
    pub focus: ScmFocus,
    pub message: String,
    pub message_cursor: usize,
    pub menu: Option<ContextMenu>,
    pub confirm: Option<PendingConfirm>,
    pub flash: Option<String>,
    /// Whether a refresh has ever landed — a never-visited panel shows a loading state rather
    /// than an empty repo (mirrors `TabStash::visited`).
    pub loaded: bool,
}

impl ScmPanel {
    pub fn rows(&self) -> &[Row] {
        &self.rows
    }

    pub fn drawer(&self, kind: DrawerKind) -> &Drawer {
        &self.drawers[kind.index()]
    }

    pub fn selected(&self) -> Option<&Row> {
        self.rows.get(self.cursor)
    }

    /// The file the selected row would open in the read pane — `None` for a header or drawer
    /// line, so a cursor landing there leaves the pane showing what it did (`App::move_cursor`'s
    /// directory-row precedent). The whole entry, since the diff needs the rename source too.
    pub fn selected_file(&self) -> Option<&FileEntry> {
        match self.selected()? {
            Row::Staged(i) => Some(&self.status.staged[*i]),
            Row::Unstaged(i) => Some(&self.status.unstaged[*i]),
            Row::StagedHeader | Row::ChangesHeader | Row::DrawerHeader(_) | Row::DrawerLine(..) => {
                None
            }
        }
    }

    /// The top row the last frame painted. Hit-testing and a drag's extent map the pointer
    /// against this — the geometry the reviewer is actually looking at — rather than a
    /// recomputed one (the PR navigator's `pr_nav_scroll` precedent).
    pub fn scroll(&self) -> usize {
        self.scroll.get()
    }

    /// The top visible row for a `viewport_height`-tall list: reveal `cursor` when a move asked
    /// for it (the smallest change that brings it on screen, same rule as the file navigator's
    /// `reveal_files` and the PR navigator's `reveal_pr_nav`), then clamp. Called once per
    /// frame, by the render.
    pub fn reveal_scroll(&self, viewport_height: usize) -> usize {
        let mut top = self.scroll.get();
        if viewport_height == 0 {
            return top;
        }
        if self.reveal.replace(false) {
            if self.cursor < top {
                top = self.cursor;
            } else if self.cursor >= top + viewport_height {
                top = self.cursor + 1 - viewport_height;
            }
        }
        top = top.min(self.rows.len().saturating_sub(viewport_height));
        self.scroll.set(top);
        top
    }

    /// Scroll the list by `delta` rows without moving the cursor — the wheel and a drag's edge
    /// scroll. The upper bound is the next frame's clamp, which is the only place that knows
    /// the viewport's height.
    pub fn wheel(&mut self, delta: isize) {
        self.scroll.set(self.scroll.get().saturating_add_signed(delta));
    }

    /// Re-read status and every expanded drawer, then rebuild the row list. Synchronous — see
    /// the module doc for why this stays inline rather than moving to a worker.
    pub fn refresh(&mut self, repo: &Path) -> Result<()> {
        let before = self.selected().map(|r| self.identity_of(r));
        self.status = porcelain::status(repo)?;
        for kind in DrawerKind::ALL {
            if self.drawers[kind.index()].expanded {
                self.drawers[kind.index()].lines = kind.fetch(repo)?;
            }
        }
        self.loaded = true;
        self.rebuild_rows();
        self.reconcile_cursor(before);
        Ok(())
    }

    fn identity_of(&self, row: &Row) -> Identity {
        match *row {
            Row::Staged(i) => Identity::Staged(self.status.staged[i].path.clone()),
            Row::Unstaged(i) => Identity::Unstaged(self.status.unstaged[i].path.clone()),
            Row::DrawerHeader(k) => Identity::DrawerHeader(k),
            Row::DrawerLine(k, i) => {
                Identity::DrawerLine(k, self.drawers[k.index()].lines[i].clone())
            }
            Row::StagedHeader | Row::ChangesHeader => Identity::DrawerHeader(DrawerKind::Graph), // unreachable in practice; headers never move
        }
    }

    /// Continuity: match the old selection by identity, else the nearest surviving row, else
    /// clamp to the list's new length.
    fn reconcile_cursor(&mut self, before: Option<Identity>) {
        if let Some(id) = before
            && let Some(pos) = self.rows.iter().position(|r| self.identity_of(r) == id)
        {
            self.cursor = pos;
            return;
        }
        self.cursor = self.cursor.min(self.rows.len().saturating_sub(1));
    }

    fn rebuild_rows(&mut self) {
        self.rows.clear();
        self.rows.push(Row::StagedHeader);
        if !self.staged_collapsed {
            self.rows.extend((0..self.status.staged.len()).map(Row::Staged));
        }
        self.rows.push(Row::ChangesHeader);
        if !self.changes_collapsed {
            self.rows.extend((0..self.status.unstaged.len()).map(Row::Unstaged));
        }
        for kind in DrawerKind::ALL {
            self.rows.push(Row::DrawerHeader(kind));
            if self.drawers[kind.index()].expanded {
                self.rows.extend(
                    (0..self.drawers[kind.index()].lines.len()).map(|i| Row::DrawerLine(kind, i)),
                );
            }
        }
    }

    /// Move the cursor to `row`, clamped to the list — a click's absolute target, as opposed
    /// to [`Self::move_by`]'s relative step.
    pub fn select(&mut self, row: usize) {
        self.cursor = row.min(self.rows.len().saturating_sub(1));
        self.reveal.set(true);
    }

    /// Move the cursor by `delta` rows, clamped to the list.
    pub fn move_by(&mut self, delta: isize) {
        if self.rows.is_empty() {
            return;
        }
        let max = self.rows.len() - 1;
        self.cursor = (self.cursor as isize + delta).clamp(0, max as isize) as usize;
        self.reveal.set(true);
    }

    fn staged_entry(&self, i: usize) -> &FileEntry {
        &self.status.staged[i]
    }
    fn unstaged_entry(&self, i: usize) -> &FileEntry {
        &self.status.unstaged[i]
    }

    /// Enter/Space on the selected row: toggle a section or drawer header, or stage/unstage a
    /// file. Nothing on a drawer line — opening one is M3's job (opening the read pane), not
    /// this panel's.
    pub fn activate(&mut self, repo: &Path) -> Result<()> {
        match self.selected().cloned() {
            Some(Row::StagedHeader) => self.staged_collapsed = !self.staged_collapsed,
            Some(Row::ChangesHeader) => self.changes_collapsed = !self.changes_collapsed,
            Some(Row::DrawerHeader(kind)) => {
                let d = &mut self.drawers[kind.index()];
                d.expanded = !d.expanded;
                if d.expanded {
                    d.lines = kind.fetch(repo)?;
                }
            }
            Some(Row::Staged(i)) => {
                porcelain::unstage(repo, self.staged_entry(i))?;
                self.flash = Some("unstaged".into());
                self.refresh(repo)?;
                // The row just crossed into the other section, under the user's own keystroke,
                // so the view follows it there.
                self.reveal.set(true);
                return Ok(());
            }
            Some(Row::Unstaged(i)) => {
                porcelain::stage(repo, self.unstaged_entry(i))?;
                self.flash = Some("staged".into());
                self.refresh(repo)?;
                self.reveal.set(true);
                return Ok(());
            }
            Some(Row::DrawerLine(..)) | None => return Ok(()),
        }
        self.rebuild_rows();
        Ok(())
    }

    pub fn stage_all(&mut self, repo: &Path) -> Result<()> {
        porcelain::stage_all(repo)?;
        self.flash = Some("staged all".into());
        self.refresh(repo)
    }

    pub fn unstage_all(&mut self, repo: &Path) -> Result<()> {
        porcelain::unstage_all(repo)?;
        self.flash = Some("unstaged all".into());
        self.refresh(repo)
    }

    pub fn commit(&mut self, repo: &Path) -> Result<()> {
        let message = self.message.trim().to_string();
        if message.is_empty() || self.status.staged.is_empty() {
            return Ok(());
        }
        let summary = porcelain::commit(repo, &message)?;
        self.message.clear();
        self.message_cursor = 0;
        self.focus = ScmFocus::List;
        self.flash = Some(summary);
        self.refresh(repo)
    }

    // ---- commit message editing ----------------------------------------------------------
    // A small char-indexed caret editor over `message`, independent of `App::input`/`caret`
    // (that pair is Mode-driven — `Composing`/`Search`/`Find`/`BasePick` — and SCM mode's
    // commit box is a tab-scoped field, not a `Mode`, so it keeps its own pair here rather
    // than growing `Mode`'s already-exhaustive matches for a fifth unrelated text field).

    fn edit_message(&mut self, f: impl FnOnce(&mut Vec<char>, &mut usize)) {
        let mut v: Vec<char> = self.message.chars().collect();
        let mut caret = self.message_cursor.min(v.len());
        f(&mut v, &mut caret);
        self.message_cursor = caret.min(v.len());
        self.message = v.into_iter().collect();
    }

    pub fn message_push(&mut self, ch: char) {
        self.edit_message(|v, caret| {
            v.insert(*caret, ch);
            *caret += 1;
        });
    }

    pub fn message_backspace(&mut self) {
        self.edit_message(|v, caret| {
            if *caret > 0 {
                v.remove(*caret - 1);
                *caret -= 1;
            }
        });
    }

    pub fn message_delete_forward(&mut self) {
        self.edit_message(|v, caret| {
            if *caret < v.len() {
                v.remove(*caret);
            }
        });
    }

    pub fn message_kill_to_start(&mut self) {
        self.edit_message(|v, caret| {
            v.drain(0..*caret);
            *caret = 0;
        });
    }

    pub fn message_caret_left(&mut self) {
        self.message_cursor = self.message_cursor.saturating_sub(1);
    }

    pub fn message_caret_right(&mut self) {
        self.message_cursor = (self.message_cursor + 1).min(self.message.chars().count());
    }

    /// The start of the caret's logical line: the whole box on a one-line message.
    pub fn message_caret_home(&mut self) {
        self.edit_message(|v, caret| *caret = crate::app::line_start(v, *caret));
    }

    /// The end of the caret's logical line.
    pub fn message_caret_end(&mut self) {
        self.edit_message(|v, caret| *caret = crate::app::line_end(v, *caret));
    }

    /// A bracketed paste at the caret, newlines kept (a commit body is several lines) and
    /// `\r\n`/`\r` normalized to `\n`.
    pub fn message_paste(&mut self, text: &str) {
        let text: Vec<char> = text.replace("\r\n", "\n").replace('\r', "\n").chars().collect();
        self.edit_message(|v, caret| {
            let n = text.len();
            v.splice(*caret..*caret, text);
            *caret += n;
        });
    }

    // ---- context menu -------------------------------------------------------------------

    /// Open the context menu for the selected row, if it has one. Drawer headers and an empty
    /// selection have no menu.
    pub fn open_menu(&mut self) {
        if let Some((target, entries)) = self.menu_for_selection() {
            self.menu = Some(ContextMenu { target, entries, selected: 0 });
        }
    }

    /// Whether the selected row has a context menu — the footer's `.` hint, so the bar never
    /// advertises a menu that would not open.
    pub fn has_menu(&self) -> bool {
        self.menu_for_selection().is_some()
    }

    /// The selected row's menu target and entries, or `None` where `.` does nothing. The one
    /// source [`Self::open_menu`] and [`Self::has_menu`] share.
    fn menu_for_selection(&self) -> Option<(MenuTarget, Vec<MenuEntry>)> {
        let row = self.selected()?.clone();
        let (target, letter) = match row {
            Row::Staged(i) => (MenuTarget::Staged(i), self.staged_entry(i).letter),
            Row::Unstaged(i) => (MenuTarget::Unstaged(i), self.unstaged_entry(i).letter),
            Row::DrawerLine(kind, i) => (MenuTarget::Drawer(kind, i), ' '),
            _ => return None,
        };
        let entries = match &target {
            MenuTarget::Staged(_) => vec![
                entry(MenuAction::Unstage),
                entry(MenuAction::Discard),
                entry(MenuAction::CopyPath),
                entry(MenuAction::CopyRelativePath),
            ],
            MenuTarget::Unstaged(_) => {
                let mut v = vec![
                    entry(MenuAction::Stage),
                    entry(MenuAction::CopyPath),
                    entry(MenuAction::CopyRelativePath),
                ];
                if letter != 'D' {
                    v.insert(1, entry(MenuAction::Discard));
                }
                v
            }
            MenuTarget::Drawer(kind, i) => self.drawer_entries(*kind, *i),
        };
        (!entries.is_empty()).then_some((target, entries))
    }

    fn drawer_entries(&self, kind: DrawerKind, i: usize) -> Vec<MenuEntry> {
        let Some(line) = self.drawers[kind.index()].lines.get(i) else { return Vec::new() };
        match kind {
            DrawerKind::Branches => {
                if line.trim_start().starts_with('*') {
                    vec![entry(MenuAction::CopyName)]
                } else {
                    vec![
                        entry(MenuAction::Checkout),
                        entry(MenuAction::MergeInto),
                        entry(MenuAction::DeleteBranch),
                        entry(MenuAction::CopyName),
                    ]
                }
            }
            DrawerKind::Stashes => {
                vec![
                    entry(MenuAction::ApplyStash),
                    entry(MenuAction::PopStash),
                    entry(MenuAction::DropStash),
                ]
            }
            DrawerKind::Tags => {
                vec![
                    entry(MenuAction::CheckoutTag),
                    entry(MenuAction::DeleteTag),
                    entry(MenuAction::CopyName),
                ]
            }
            DrawerKind::Remotes => {
                vec![entry(MenuAction::FetchRemote), entry(MenuAction::CopyName)]
            }
            DrawerKind::Worktrees => {
                vec![entry(MenuAction::CopyPath), entry(MenuAction::RemoveWorktree)]
            }
            DrawerKind::Graph | DrawerKind::Commits => vec![entry(MenuAction::CopyName)],
        }
    }

    pub fn close_menu(&mut self) {
        self.menu = None;
    }

    pub fn menu_move(&mut self, delta: isize) {
        if let Some(menu) = &mut self.menu
            && !menu.entries.is_empty()
        {
            let max = menu.entries.len() as isize - 1;
            menu.selected = (menu.selected as isize + delta).clamp(0, max) as usize;
        }
    }

    /// Fire the selected menu entry: a destructive one becomes a [`PendingConfirm`], everything
    /// else runs immediately.
    pub fn menu_confirm(&mut self, repo: &Path) -> Result<Option<String>> {
        let Some(menu) = self.menu.take() else { return Ok(None) };
        let Some(chosen) = menu.entries.get(menu.selected) else { return Ok(None) };
        if chosen.action.destructive() {
            self.confirm = Some(PendingConfirm {
                message: format!("{}?", chosen.action.label().trim_end_matches('…')),
                target: menu.target,
                action: chosen.action,
            });
            return Ok(None);
        }
        self.run_menu_action(repo, &menu.target, chosen.action)
    }

    pub fn confirm_yes(&mut self, repo: &Path) -> Result<Option<String>> {
        let Some(pending) = self.confirm.take() else { return Ok(None) };
        self.run_menu_action(repo, &pending.target, pending.action)
    }

    pub fn confirm_no(&mut self) {
        self.confirm = None;
    }

    /// Run one menu action against `target`. Returns clipboard text for a copy action, so the
    /// caller (which owns the clipboard tool) can export it; every other action mutates the
    /// repo directly and refreshes.
    fn run_menu_action(
        &mut self,
        repo: &Path,
        target: &MenuTarget,
        action: MenuAction,
    ) -> Result<Option<String>> {
        match (target, action) {
            (MenuTarget::Staged(i), MenuAction::Unstage) => {
                porcelain::unstage(repo, self.staged_entry(*i))?;
            }
            (MenuTarget::Unstaged(i), MenuAction::Stage) => {
                porcelain::stage(repo, self.unstaged_entry(*i))?;
            }
            (MenuTarget::Staged(i), MenuAction::Discard) => {
                porcelain::discard(repo, self.staged_entry(*i))?;
            }
            (MenuTarget::Unstaged(i), MenuAction::Discard) => {
                porcelain::discard(repo, self.unstaged_entry(*i))?;
            }
            (MenuTarget::Staged(i), MenuAction::CopyPath) => {
                return Ok(Some(abs_path(repo, self.staged_entry(*i))));
            }
            (MenuTarget::Unstaged(i), MenuAction::CopyPath) => {
                return Ok(Some(abs_path(repo, self.unstaged_entry(*i))));
            }
            (MenuTarget::Staged(i), MenuAction::CopyRelativePath) => {
                return Ok(Some(self.staged_entry(*i).path.clone()));
            }
            (MenuTarget::Unstaged(i), MenuAction::CopyRelativePath) => {
                return Ok(Some(self.unstaged_entry(*i).path.clone()));
            }
            (MenuTarget::Drawer(kind, i), action) => {
                return self.run_drawer_action(repo, *kind, *i, action);
            }
            _ => {}
        }
        self.flash = Some(action.label().trim_end_matches('…').to_string());
        self.refresh(repo)?;
        Ok(None)
    }

    fn run_drawer_action(
        &mut self,
        repo: &Path,
        kind: DrawerKind,
        i: usize,
        action: MenuAction,
    ) -> Result<Option<String>> {
        let Some(line) = self.drawers[kind.index()].lines.get(i).cloned() else { return Ok(None) };
        let name = drawer_ref_name(kind, &line);
        match action {
            MenuAction::CopyName | MenuAction::CopyPath => return Ok(Some(name)),
            MenuAction::Checkout | MenuAction::CheckoutTag => {
                crate::git::run(repo, &["checkout", &name])?;
            }
            MenuAction::MergeInto => {
                crate::git::run(repo, &["merge", "--no-edit", &name])?;
            }
            MenuAction::DeleteBranch => {
                crate::git::run(repo, &["branch", "-D", &name])?;
            }
            MenuAction::ApplyStash => {
                crate::git::run(repo, &["stash", "apply", &name])?;
            }
            MenuAction::PopStash => {
                crate::git::run(repo, &["stash", "pop", &name])?;
            }
            MenuAction::DropStash => {
                crate::git::run(repo, &["stash", "drop", &name])?;
            }
            MenuAction::DeleteTag => {
                crate::git::run(repo, &["tag", "-d", &name])?;
            }
            MenuAction::FetchRemote => {
                crate::git::run(repo, &["fetch", &name])?;
            }
            MenuAction::RemoveWorktree => {
                crate::git::run(repo, &["worktree", "remove", &name])?;
            }
            MenuAction::Stage
            | MenuAction::Unstage
            | MenuAction::Discard
            | MenuAction::CopyRelativePath => {}
        }
        self.flash = Some(action.label().trim_end_matches('…').to_string());
        self.refresh(repo)?;
        Ok(None)
    }
}

fn abs_path(repo: &Path, entry: &FileEntry) -> String {
    repo.join(&entry.path).display().to_string()
}

fn entry(action: MenuAction) -> MenuEntry {
    MenuEntry { action, label: action.label() }
}

/// The name/hash a drawer's raw display line resolves to, for the git command a menu action
/// runs. Each drawer's line format is fixed by the `git log --format`/`branch --format` call
/// that built it ([`DrawerKind::fetch`]).
fn drawer_ref_name(kind: DrawerKind, line: &str) -> String {
    match kind {
        DrawerKind::Branches => line
            .trim_start()
            .trim_start_matches('*')
            .split_whitespace()
            .next()
            .unwrap_or("")
            .to_string(),
        DrawerKind::Stashes => line.split(':').next().unwrap_or(line).trim().to_string(),
        DrawerKind::Tags | DrawerKind::Remotes | DrawerKind::Worktrees => {
            line.split_whitespace().next().unwrap_or(line).to_string()
        }
        DrawerKind::Graph | DrawerKind::Commits => line
            .trim_start_matches(|c: char| !c.is_ascii_hexdigit())
            .split_whitespace()
            .next()
            .unwrap_or(line)
            .to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry_at(path: &str, letter: char) -> FileEntry {
        FileEntry { path: path.into(), orig: None, letter }
    }

    fn status(staged: &[&str], unstaged: &[&str]) -> Status {
        Status {
            branch: "main".into(),
            staged: staged.iter().map(|p| entry_at(p, 'M')).collect(),
            unstaged: unstaged.iter().map(|p| entry_at(p, 'M')).collect(),
            ahead: 0,
            behind: 0,
            has_upstream: true,
        }
    }

    fn panel(staged: &[&str], unstaged: &[&str]) -> ScmPanel {
        let mut p = ScmPanel { status: status(staged, unstaged), ..ScmPanel::default() };
        p.rebuild_rows();
        p
    }

    #[test]
    fn rebuild_rows_orders_staged_then_changes_then_drawers() {
        let p = panel(&["a.rs"], &["b.rs", "c.rs"]);
        assert_eq!(
            p.rows(),
            &[
                Row::StagedHeader,
                Row::Staged(0),
                Row::ChangesHeader,
                Row::Unstaged(0),
                Row::Unstaged(1),
                Row::DrawerHeader(DrawerKind::Graph),
                Row::DrawerHeader(DrawerKind::Commits),
                Row::DrawerHeader(DrawerKind::Branches),
                Row::DrawerHeader(DrawerKind::Remotes),
                Row::DrawerHeader(DrawerKind::Stashes),
                Row::DrawerHeader(DrawerKind::Tags),
                Row::DrawerHeader(DrawerKind::Worktrees),
            ]
        );
    }

    #[test]
    fn a_collapsed_section_hides_its_rows_but_keeps_its_header() {
        let mut p = panel(&["a.rs"], &["b.rs"]);
        p.staged_collapsed = true;
        p.rebuild_rows();
        assert!(p.rows().contains(&Row::StagedHeader));
        assert!(!p.rows().contains(&Row::Staged(0)));
        assert!(p.rows().contains(&Row::Unstaged(0)));
    }

    #[test]
    fn move_by_clamps_at_both_ends() {
        let mut p = panel(&[], &["a.rs"]);
        let max = p.rows().len() - 1;
        p.move_by(-5);
        assert_eq!(p.cursor, 0);
        p.move_by(1000);
        assert_eq!(p.cursor, max);
    }

    #[test]
    fn the_wheel_moves_the_viewport_alone_and_the_frame_clamps_it() {
        // Twelve rows in a four-row window: the wheel scrolls without touching the cursor, and
        // the clamp stops the top where the last row still fills the window (no blank tail).
        let mut p = panel(&["a.rs"], &["b.rs", "c.rs"]);
        let rows = p.rows().len();
        assert_eq!(p.reveal_scroll(4), 0);

        p.wheel(3);
        assert_eq!(p.reveal_scroll(4), 3, "the wheel moved the top");
        assert_eq!(p.cursor, 0, "and left the cursor where it was");

        p.wheel(-9);
        assert_eq!(p.reveal_scroll(4), 0, "an over-scroll up stops at the first row");
        p.wheel(1000);
        assert_eq!(p.reveal_scroll(4), rows - 4, "and down at the last full window");
    }

    #[test]
    fn only_a_cursor_move_pulls_a_wheeled_viewport_back() {
        let mut p = panel(&["a.rs"], &["b.rs", "c.rs"]);
        p.wheel(5);
        assert_eq!(p.reveal_scroll(4), 5);

        // A rebuild is not the user's input: the viewport stays where it was left (Continuity).
        let before = p.selected().map(|r| p.identity_of(r));
        p.rebuild_rows();
        p.reconcile_cursor(before);
        assert_eq!(p.reveal_scroll(4), 5, "a refresh does not yank the view back");

        // The next cursor move does, and the reveal is spent on that one frame.
        p.move_by(1);
        assert_eq!(p.reveal_scroll(4), 1, "the move revealed the cursor");
        p.wheel(4);
        assert_eq!(p.reveal_scroll(4), 5, "the reveal did not repeat");
    }

    #[test]
    fn cursor_follows_its_file_by_identity_across_a_reorder() {
        // Simulate what `refresh` does: capture identity, rebuild with a different file order
        // (as a real status re-read would, once a file sorts differently), reconcile.
        let mut p = panel(&[], &["a.rs", "b.rs"]);
        p.cursor = 3; // StagedHeader, ChangesHeader, Unstaged(0)="a.rs", Unstaged(1)="b.rs"
        let before = p.selected().map(|r| p.identity_of(r));
        p.status = status(&[], &["b.rs", "a.rs"]); // reordered
        p.rebuild_rows();
        p.reconcile_cursor(before);
        assert_eq!(p.selected(), Some(&Row::Unstaged(0)));
        assert_eq!(p.status.unstaged[0].path, "b.rs");
    }

    #[test]
    fn cursor_falls_back_to_clamp_when_its_row_is_gone() {
        let mut p = panel(&[], &["a.rs", "b.rs"]);
        p.cursor = 3; // "b.rs"
        let before = p.selected().map(|r| p.identity_of(r));
        p.status = status(&[], &["a.rs"]); // "b.rs" staged/removed elsewhere
        p.rebuild_rows();
        p.reconcile_cursor(before);
        assert!(p.cursor < p.rows().len());
    }

    #[test]
    fn unstaged_menu_offers_discard_unless_the_file_is_deleted() {
        let mut p = panel(&[], &["a.rs"]);
        p.cursor = 2; // StagedHeader, ChangesHeader, Unstaged(0)="a.rs"
        p.open_menu();
        let actions: Vec<_> = p.menu.as_ref().unwrap().entries.iter().map(|e| e.action).collect();
        assert!(actions.contains(&MenuAction::Discard));

        p.status.unstaged[0].letter = 'D';
        p.close_menu();
        p.open_menu();
        let actions: Vec<_> = p.menu.as_ref().unwrap().entries.iter().map(|e| e.action).collect();
        assert!(!actions.contains(&MenuAction::Discard));
    }

    #[test]
    fn a_destructive_action_stages_a_confirm_instead_of_running() {
        let mut p = panel(&["a.rs"], &[]);
        p.cursor = 1; // staged a.rs
        p.open_menu();
        let idx = p
            .menu
            .as_ref()
            .unwrap()
            .entries
            .iter()
            .position(|e| e.action == MenuAction::Discard)
            .unwrap();
        p.menu.as_mut().unwrap().selected = idx;
        // No repo needed: destructive actions short-circuit before touching git.
        let repo = Path::new("/nonexistent");
        let out = p.menu_confirm(repo).unwrap();
        assert!(out.is_none());
        assert!(p.menu.is_none());
        assert_eq!(p.confirm.as_ref().unwrap().action, MenuAction::Discard);
    }

    #[test]
    fn branch_drawer_hides_checkout_and_merge_for_the_current_branch() {
        let mut p = ScmPanel::default();
        p.drawers[DrawerKind::Branches.index()] =
            Drawer { expanded: true, lines: vec!["* main".into(), "  feature/x".into()] };
        p.rebuild_rows();
        p.cursor =
            p.rows().iter().position(|r| *r == Row::DrawerLine(DrawerKind::Branches, 0)).unwrap();
        p.open_menu();
        let actions: Vec<_> = p.menu.as_ref().unwrap().entries.iter().map(|e| e.action).collect();
        assert_eq!(actions, vec![MenuAction::CopyName]);

        p.close_menu();
        p.cursor =
            p.rows().iter().position(|r| *r == Row::DrawerLine(DrawerKind::Branches, 1)).unwrap();
        p.open_menu();
        let actions: Vec<_> = p.menu.as_ref().unwrap().entries.iter().map(|e| e.action).collect();
        assert!(actions.contains(&MenuAction::MergeInto));
        assert!(actions.contains(&MenuAction::Checkout));
    }

    #[test]
    fn drawer_ref_name_strips_the_current_branch_marker() {
        assert_eq!(drawer_ref_name(DrawerKind::Branches, "* main"), "main");
        assert_eq!(drawer_ref_name(DrawerKind::Branches, "  feature/x"), "feature/x");
    }

    #[test]
    fn commit_refuses_an_empty_message_or_nothing_staged() {
        let mut p = panel(&[], &["a.rs"]); // nothing staged
        p.message = "a message".into();
        let repo = Path::new("/nonexistent");
        // Nothing staged: refuses before touching git, so this must not error.
        p.commit(repo).unwrap();
        assert_eq!(p.message, "a message"); // untouched, never cleared
    }
}
