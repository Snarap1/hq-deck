# AGENTS.md

hq-deck is a Rust TUI (ratatui) review and source-control tool for one git worktree.
It runs in a terminal beside a coding agent, shows the worktree's diff, and takes line comments.
It sends the comments to the agent through herdr-core's `hc send`, or copies them to the clipboard.
It stages, commits, and runs git operations on the same worktree.
One binary, one git worktree per process (`hq-deck [repo]`).

## Ground rules

- Linux only.
  Do not add macOS or Windows branches.
- No release binaries, no publishing pipeline, no CI.
  `cargo install --path .` (`just install`) is the whole distribution story, and `just ci` is the whole check.
- The binary has no herdr-app dependency: it never calls `herdr-app` and works in any terminal.
  herdr-core (`hc`) and the older `herdr` are the agent transports; `src/agent.rs` reaches each only through its CLI.
  The herdr-app integration lives outside the binary, in `integrations/herdr/` (a mod plus its key), and the older herdr's in `integrations/herdr-legacy/` (a plugin), both installed by `just install-herdr`.
- There is no upstream sync.

## Commands

- `just test` - full test suite.
  Single test: `cargo test <name>`; integration tests live in `tests/`: `cargo test --test app_flow <name>`.
- `just lint` - clippy with warnings as errors.
  `just fmt` / `just fmt-check` - rustfmt.
- `just ci` - fmt-check, lint, test, release build.
  Run it before calling a change done.
- `just install` - `cargo install --path . --locked` into `~/.cargo/bin/hq-deck`.
  Cargo replaces the binary through a fresh inode, so a running hq-deck keeps the old image until restarted.
- `just install-herdr` - copies `integrations/herdr/mods/hq-deck` into `~/.config/herdr-app/mods` and merges `integrations/herdr/keys.json` into the user's `keys.json`.
  It refuses to replace a key already bound to something else.
  With `herdr` installed it also runs `herdr plugin link integrations/herdr-legacy`; either host alone is enough.
- `just smoke-edit` - PTY smoke test of the editor path (`e`) against a real release binary.
  Unit tests stop at the argv, so run it after any change to `run_editor`, the terminal mode stack, or the editor dialects.
  It is not part of `just ci` and takes about a minute.
  Run it detached: `setsid python3 scripts/smoke_edit_file.py --binary target/release/hq-deck < /dev/null`.
  crossterm sizes the pane from `/dev/tty`, so inside a short pane the below-the-fold checks fail for no reason of the code's.
  The same caveat applies to `scripts/bench_tui.py`.
- `python3 scripts/bench_tui.py --binary target/release/hq-deck --fixture` - perceived-latency benchmark (keypress to painted frame, via PTY), the acceptance instrument.
  `cargo run --release --example bench_latency -- <repo>` attributes a slow number to its component calls.
  The one committed baseline is `scripts/bench-results/baseline.json`; replace it when a change moves the numbers, never add per-round runs.
  Run before and after any change to the reload, render, git, or highlight paths.
  The `editor_*` scenarios (open by `e`, `pgdn`, `ctrl+f` open, a query matching every line, `enter` between matches, a typed character mid-file) run in a second session on the fixture's `src/generated.rs` and cover the built-in editor's render and incremental highlight.
  Run it detached too (`setsid ... < /dev/null`), since the exit code is 1 when a scenario never reached the editor or the file changed on disk; the script never presses `ctrl+s` and discards (`d`) an edit on close.
  For a real repo, `--editor-file <path>` picks the big file; without it `--repo` skips them.
  Compare medians A/B under the same load: build the old binary to a second target dir and interleave runs.

## Invariants

Load-bearing. Cite them by name.

- **Writes are asked for**: every mutation of the repository is the direct result of a keystroke that named it.
  The places that make them are `Changes`' SCM mode (stage, unstage, discard, commit, and the `.` menu's checkout/stash/tag/branch/worktree entries), `GitOps` (pull/push/fetch/merge/rebase), and the built-in editor's `ctrl+s` (one worktree file, `EditSession::save`).
  Destructive ones confirm first, and the editor's save never overwrites a file that changed on disk since it was read without asking (`o`/`r`).
  Everywhere else deck reads: the diff, `All files`, search, preview, and the `PR` tab (which never posts to a forge) mutate nothing.
  No path mutates on a poll, a refresh, or a fetch landing.
  The only other git writes are two worktree-private refs, `refs/worktree/reviewr/{base-pick,turn-base}`, written by `B`'s pick and `M`'s mark.
  The namespace keeps its old spelling so existing worktrees keep their pick.
  The boundary must not drift further without an explicit decision.
- **Comments survive**: comments are never lost to a refresh or the agent's edits, and leave only by an explicit successful export (`s` send, `y` copy).
  The comment store is in-memory by design - do not propose persisting it.
  A content comment's lines, and a diff comment's on the new side of a worktree scope, follow their file (`CommentStore::rebase`, by line diff from the text they count in): an edit never drops one, and one whose lines were all deleted stays on the line above the gap, `orphaned`, with its snippet.
  The built-in editor's unsaved text holds to the same rule: no poll, refresh, agent write, or config recovery touches the buffer, and it leaves only by a save, an explicit discard (`d`), or a reload the user answered (`r`).
  The one quit reachable while it is open, the blocked config screen's `q`, says on that screen that it discards the edit.
- **Continuity**: place state (cursor, scroll, tab, scope, folds, selection, layout) moves only under the user's own input.
  World events (polls, refreshes, fetch results) may only reconcile it: match by identity first (path, comment author+anchor, never row index), fall back to the nearest surviving target, clamp last.
  Derived state on screen may be stale, never wrong: blank a view only when its identity changed, never because the same thing gained newer content.
  An old-side diff comment counts in the diff's base (HEAD, merge-base, the turn snapshot), which a worktree edit does not move, and a `Rev::Commit` comment in its commit: neither holds a basis, so neither moves.
  A file a read pane paints (the open view, or the other file tab's stash) carries its comments only with that view's own rebuild, so a held view's cards stay on the rows it painted; the poll carries every other file (`App::carry_comments`), and an export carries copies to the disk.

## Architecture

The runtime is a single-threaded frame loop (`event_loop` in `src/lib.rs`): draw, wait for input or a poll deadline, mutate `App`, draw.
Clipboard, agent send, per-file diff builds, and the SCM-mode/`GitOps` status reads run synchronously between frames.
A terminal editor holds the loop for its whole session by design (`policies/ux-responsiveness.md`).

Seven things run on worker threads: the world worker (`src/world.rs`), the search worker (`src/search.rs`, fff-search with its own scan, watch, and content-index threads), the PR input probe, the PR forge fetch (`gh`/`glab`), config recovery, the commit-message generator (`src/commit_msg.rs`), and one git operation at a time (`src/git/ops.rs`).
The last two share a shape: `App` holds a pending flag (`scm_suggesting`, `gitops.running`, `gitops_dispatch`) and the `mpsc::Receiver` lives as a local in `event_loop`, because `App` derives `Debug` and a receiver does not.
World results land through `land_world_completion`: input-tagged, latest-wins, reconciled only while the view still matches.

### Modules

- `src/app.rs` - the `App` state machine.
  Tabs: `Changes`/`AllFiles`/`Pr`/`GitOps`; `Changes` has a second navigator face, SCM mode (`scm_open` holds the scope it left, `scm_active()` is the one test).
  Scopes: `Uncommitted`/`Branch`/`LastTurn`/`Commits`.
  `Focus` is files vs diff pane.
  `Mode` is `Normal`, the `Composing`/`List`/`BasePick`/`CommitPick` overlays, the `Editing` built-in editor over the read pane, the `Find` band, and the body-replacing `Search` screen.
  The SCM-mode and `GitOps` overlays are deliberately not `Mode` variants; they are fields on their tab's panel.
  `reconcile_world()` is the one place a world snapshot touches place state.
  `reload()` is the synchronous build+reconcile pair used at startup, first tab visits, and scope switches.
  Each file tab stashes its full place state on switch-away (`swap_active_with_stash`); `Pr`, SCM mode, and `GitOps` own their state outside that stash.
  While composing, the open diff is frozen (reconcile skips it) so a draft's anchor cannot move.
  `GitOpsPanel` lives here: five actions, a picker, one confirm.
- `src/lib.rs` - the frame loop and all key/mouse dispatch.
  `Pr`, SCM mode, and `GitOps` each run their own key block (overlays first, then the tab's list) instead of growing `Mode`.
  `switch_tab` is the one place a tab key is honored, so the tab row works from inside those blocks too.
- `src/keymap.rs` - the rebindable action table (`ACTIONS`: action, config name, default keys) and `[keybindings]` resolution.
  A new action is a row there plus its dispatch arm in `lib.rs`.
- `src/world.rs` - the world worker: the pure snapshot build (`WorldInput` -> `WorldSnapshot`) and latest-wins request/completion channels.
  Each build rereads the `last-turn` mark from its ref, like the base pick.
- `src/git/` - every git subprocess, behind one module (`mod.rs` owns the runners and re-exports `scope::*`, so callers say `crate::git::...`).
  - `scope.rs` - the review scopes: `changed_files`, `all_files`, `snapshot_worktree` (temp-index `add -A` + `write-tree` for the `M` mark), baseline refs.
    `all_files` is tracked + untracked + ignored via `ls-files`; never use `git status --ignored`, it walks inside ignored trees and costs seconds.
  - `porcelain.rs` - the staging layer: the `--porcelain=v1 -b` status parse (ahead/behind/upstream, conflict marker), stage/unstage/discard/commit, `diff_for_message`.
  - `drawers.rs` - the read-only lists behind the SCM drawers (graph, commits, branches, remotes, stashes, tags, worktrees).
  - `ops.rs` - the `Git ops` table (update/push/fetch-all/merge/rebase) as an argv builder plus a background `spawn`.
  - No `ratatui` in `porcelain.rs`, `drawers.rs`, or `ops.rs`.
- `src/scm_panel.rs` - SCM mode's whole state: Staged/Changes sections, the commit box's char-indexed buffer, the seven drawers, the context menu, the pending confirm.
  `App` holds one field (`scm`) and delegating methods; keep SCM state here, not in `app.rs`.
- `src/commit_msg.rs` - the ✧ generator: `spawn(repo, config)` reads the staged diff and runs `claude -p` on a worker thread.
  Any failure returns a filename summary rather than an error.
- `src/commit_template.rs` - the branch template (`branch_pattern` + `template`): validated at config load, rendered around the generator's summary line when the branch matches.
  Pure.
- `src/icons.rs` - the file-type and status glyphs the SCM navigator paints.
- `src/diff.rs` - `FileDiff` build (syntect highlight both sides, similar-line pairing, word emphasis, folds) and `DiffCache`, keyed by path and gated by content hash.
  Cleared on scope switch and theme change.
- `src/ui.rs` - all rendering, including the SCM navigator (`render_scm_panel`, its overlay, `hit_scm_row`), the `Git ops` panel, and the footer.
  Row heights and wrapping recompute per frame across the visible diff, so render cost scales with open-file size.
  A body narrower than 60 columns drops the read pane and gives the SCM navigator the whole width.
  The footer's row 1 has room for one comment action (`s send N`); `tests/render.rs` pins that it never drops at narrow widths.
- `src/forge.rs` + the `PrRefresh`/`PrCoordinator` state machines in `lib.rs` - the PR snapshot.
  Fetches are tagged with the input that produced them (repository identity, pinned HEAD and base, the branch's forge names), and a result paints only if a fresh probe proves the input still matches.
  This generation/input-tag pattern is the template for moving other derived state off-thread.
- `src/gitlab.rs` - the `glab` provider behind the forge boundary, mapping its payloads onto the one `PrSnapshot` shape.
  GitHub and GitLab are the only forges.
- `src/model.rs` - `CommentStore` (in-memory) and comment anchoring.
  `diff_anchored` separates diff comments from content comments; each renders only in its own view.
  A content comment, and a diff comment on the new side of a worktree scope (`Rev::Worktree`), holds its `Basis`, the worktree text its numbers count in, taken from the `FileDiff::worktree` it was made over; `rebase` carries it to a newer version and is pure.
  `line_in` lets a collapsed fold cover what its hidden lines do, so a carried comment that lands in a fold keeps its card under the marker.
  A diff row (`Changes`, file tree or SCM mode, which pins the `uncommitted` scope) makes a diff comment.
  The File view and the markdown preview paint current worktree content, so they make content comments.
  Staging changes neither: the `uncommitted` diff is `HEAD` vs the worktree, which a stage does not move.
- `src/editor.rs` - the external editor command from the `editor` key: a name-keyed dialect table for a bare command, quote-aware splitting, and the `{file}`/`{line}` template.
  `$EDITOR`/`$VISUAL` are never read: with no `editor` key, `e` opens the built-in editor.
  Pure argv resolution; `run_editor` in `lib.rs` picks built-in or external, owns the spawn, and hands the pane over for a terminal editor.
- `src/edit_buffer.rs` - the built-in editor's text: lines, cursor, selection, undo/redo, and the file's line ending and final newline.
  Pure; every mutation goes through one `replace` primitive, which records the undo step and the line `Splice` the highlighter replays.
  A file a round trip could corrupt (binary, not UTF-8, mixed `\n`/`\r\n`) is refused at `parse`.
- `src/edit_session.rs` - one file open in the built-in editor: the buffer, its scroll, the disk check, the pending question, and the save.
  `App` holds one field (`edit`) and delegating methods; keep editor state here, not in `app.rs`.
  The per-line highlight (`highlight::LineHighlights`) lives in a thread-local keyed by session, not in the session: syntect's parser state is `!Send` under oniguruma, and `App` must be `Send` for config recovery.
  The cache is derived, so a missing entry is rebuilt from the text.
  Keys are fixed (micro's) and dispatched by `handle_edit_key` in `lib.rs`, not the keymap; only `e` itself is rebindable.
  Its `ctrl+f` find band (`EditFind`) types into a query through the shared field ops and only moves the selection: while it is open `act` drops every text-changing gesture, so its matches stay true.
- `src/export.rs` - comment formatting (`format_all`) and the `ExportTarget` trait, plus the clipboard target: `wl-copy`/`xclip`/`xsel` on a local display, OSC 52 over ssh or with no display (and as the fallback when a tool fails).
  OSC 52 has no acknowledgement, so it counts as success once written to a terminal stdout.
  `App::export` consumes comments only on `Ok`.
  `read_clipboard` is the editor's `ctrl+v`: the paired paste tools on a local display, `None` elsewhere (OSC 52 cannot be read back), where the editor pastes its own register.
- `src/agent.rs` - the `send` target over herdr-core or the older herdr.
  `Host` chooses per send: `$HC_TERMINAL_ID` means `hc`, else `$HERDR_PANE_ID` means herdr, else `hc`.
  It picks the agent from `hc ls --json` or `herdr agent list`: live, local (no `machine/` prefix), reporting status (OSC 7501, hc's only notion of an agent; herdr's list holds only agents), not hq-deck's own terminal, with `cwd` at or under the worktree.
  Several candidates narrow to the one in hq-deck's own `where` tab; still several is refused by name.
  It types with `hc send` without `--force`, which submits and refuses a blocked agent.
  Under herdr it checks for `blocked` itself, pastes with `pane send-text` (no Enter), and focuses the agent; hc has no paste without Enter.
  The pick is recomputed per send, never cached.
  Parse and pick are pure and unit-tested.
- `src/config.rs` - config (`$HQ_DECK_CONFIG_DIR`, else `~/.config/hq-deck/config.toml`): the whole file validates before every frame and action.
  An invalid config blocks review work until recovery, which carries authored state.
  A repo's `.hq-deck.toml` merges over `[commit_message]` per key; it is read on demand, outside the per-frame recovery gate, so a typo there cannot block review.
- `src/proc.rs` - locating external tools: `command` for hq-deck's own tools (host bins first), `user_command` for the reviewer's.

## Agent skills

### Issue tracker

Local markdown under `.scratch/<feature-slug>/`, tracked in git; triage state is the `Status:` line of a ticket file.
See `docs/agents/issue-tracker.md`.

### Domain docs

Single-context: `GLOSSARY.md` and `docs/adr/` at the root, created lazily by `/domain-modeling`.
See `docs/agents/domain.md`.
