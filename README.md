# hq-deck

A terminal review and source-control TUI for a git worktree.
Your agent writes the code.
You read its diff in a terminal beside it, comment on the lines, send the notes to the agent, then stage and commit what you kept.

<p align="center">
  <a href="#install">install</a> · <a href="#quick-start">quick start</a> · <a href="#controls">controls</a> · <a href="#the-four-tabs">tabs</a> · <a href="#diff-scopes">scopes</a> · <a href="#configuration">configuration</a> · <a href="#limitations">limitations</a>
</p>

One binary, pointed at a git worktree:

- **Diff review** - the changed files, syntax-highlighted.
- **Four diff scopes** - uncommitted, branch, last turn (since your mark), commits.
- **Line comments** - comment on a line or a range, then copy them all to the clipboard for the agent.
- **Staging** - a `git status` panel: stage, unstage, discard, commit, and the usual branch, stash, tag, and worktree lists.
- **AI commit messages** - one key writes the message for what you staged, on your own prompt.
- **Git ops** - update, push, fetch all, merge, rebase, each behind a confirm.
- **Text selection** - drag over any text to copy it, like an editor.
- **File viewer** - any file's current content from the whole worktree.
- **Search** - fuzzy file names and live code grep across the worktree, powered by [fff](https://github.com/dmtrKovalenko/fff).
- **Find in file** - search the open file and step between every match.
- **PR view** - the branch's pull request, read-only.
- **Markdown preview** - flip a `.md` file between source and rendered view, and comment on it.
- **Themes** - 18 palettes in dark and light.

It writes to your repository only where you ask it to, and only on a keystroke of yours: staging, discard, and commit in **Changes**' SCM mode, `M` (mark), and the five operations in **Git ops**.
Review, files, search, preview, and the **PR** tab never write anything - the PR tab reads GitHub or GitLab and never posts.
Besides the SCM writes, hq-deck writes exactly two refs, `refs/worktree/reviewr/base-pick` and `refs/worktree/reviewr/turn-base`, which are private to the worktree and touch no branch, no index, and no file.

## Requirements

- **git** on `PATH`.
- A **truecolor** terminal with Unicode box-drawing.
- **Linux.**
- A **Rust toolchain** - it is installed from source.
- For `s` and `alt+s`, one of: **`hc`** ([herdr-core](https://github.com/herdrdev/herdr-core)) on `PATH`, with the agent running in an `hc` terminal; or hq-deck running in a pane of the older **herdr** (0.7.5 or newer).
- On a local display, a clipboard tool: `wl-copy`, `xclip`, or `xsel`. `y` and `alt+y` copy through it.
  Over ssh, on a remote `hc` machine, or with no display at all, they copy through the terminal with OSC 52 instead, so the terminal you look at must support it (herdr-app, kitty, WezTerm, foot, Alacritty, and tmux with `set-clipboard on` do).
- **`gh`** (GitHub) or **`glab`** (GitLab), authenticated. Only the **PR** tab needs one.
- **`claude`** on `PATH`. Only the commit box's ✧ message generator needs it.

## Install

```bash
git clone <this repo> hq-deck
cd hq-deck
cargo install --path . --locked   # or: just install
```

That puts one self-contained binary at `~/.cargo/bin/hq-deck`.
Run it in any repository:

```bash
hq-deck              # the current directory's worktree
hq-deck ~/some/repo
```

**To update**, pull and install again; a running hq-deck keeps the old binary until you restart it.

### In Herdr

In [Herdr](https://github.com/herdrdev/herdr-app), one key opens hq-deck in a pane to the right of the focused one, on that terminal's worktree, and pressed again closes it:

```bash
just install-herdr
```

It installs the `hq-deck` mod into `~/.config/herdr-app/mods` and binds `super-y` to it (needs `jq`).
It will not replace a key you already use; change the key in `integrations/herdr/keys.json` and run it again.

The same recipe links the plugin in `integrations/herdr-legacy/` into the older `herdr` when it is installed.
Bind its `snarap.hq-deck.toggle` action to a key in your herdr config.

## Quick start

Run hq-deck in a terminal split next to your agent:

1. **Mark.** On the **last turn** scope (`t`), press `M` before you hand the agent a task: the scope then shows exactly what changed after the mark.
2. **Pick a file.** Changed files are in the navigator. `j` / `k` moves, the diff follows. Or `]` walks the changes hunk by hunk, file after file.
3. **Focus the diff.** `Tab` switches panes.
4. **Select lines.** `v`, then `j` / `k` to extend (or click or drag the gutter).
5. **Comment.** `c`, type, `Enter`.
6. **Send.** `s` types every comment into the agent.
   Under `hc` that is `hc send`, which submits; in a herdr pane the text is pasted without Enter and the agent is focused, so you submit it.
   The agent is the one working in this worktree; with several, the one in hq-deck's own tab.
   A blocked agent is never typed into. `y` copies them to the clipboard instead.
7. **Keep it.** `x` flips **Changes** into SCM mode: `Enter` stages the file under the cursor, `c` focuses the commit box (on an empty box it also asks ✧ for a message), `Enter` commits.
   `x` again goes back to the file tree.

The footer shows the next step. Press `?` for every key that works right now.

## Controls

The keys below are defaults. You can rebind every action, even to several keys at once
([Keybindings](#keybindings)).

**Getting around**

| Key | Action |
| --- | --- |
| `1` `2` `3` `4` | Switch tab — Changes / All files / PR / Git ops |
| `x` | On **Changes**: flip the navigator between the file tree and SCM mode |
| `u` `b` `t` `g` | Switch scope — uncommitted / branch / last turn / commits |
| `B` | Pick the base branch |
| `G` | Pick the commits to review |
| `j` `k` · `↑` `↓` | Move cursor |
| `]` `[` | Jump to next / previous hunk |
| `f` `F` | Jump to next / previous file |
| `PageUp` `PageDown` | Move a page |
| `Ctrl+U` `Ctrl+D` | Move a half-page |
| `Tab` | Switch focus |
| `→` `←` | Expand / collapse, or scroll sideways |
| `/` | Search files and code |
| `Alt+Y` | Copy the file's path (a file row in the navigator, or a search result) |
| `Alt+S` | Send that path to the agent |
| `Ctrl+F` | Find in file |
| `w` | Toggle line wrap |
| `M` | Mark the worktree as the **last turn** baseline |
| `m` | Preview markdown file |
| `p` | Rotate navigator |
| `z` | Hide / show navigator |
| `<` `>` | Grow / shrink navigator |
| `r` | Refresh |
| `?` | Open shortcuts helper |
| `q` | Quit |

**Reviewing** (in the diff)

| Key | Action |
| --- | --- |
| `v` | Select lines |
| `c` | Comment on line or selection |
| `e` | Edit the comment under the cursor, or the file (built-in editor, or your `editor`) |
| `d` | Delete comment |
| `n` `N` | Jump to next / previous comment |
| `l` | List all comments |
| `s` | Send comments to the agent |
| `y` | Copy comments to clipboard |
| `esc` | Clear selection |

**In the comment box**

| Key | Action |
| --- | --- |
| `Enter` | Save comment |
| `Esc` | Cancel |
| `Shift+Enter` · `Alt+Enter` · `Ctrl+J` | Insert newline |

Plus the usual caret moves: arrows, `Home` / `End`, `Ctrl+A` / `Ctrl+E`, `Alt+b` / `Alt+f` word
jumps, and `Ctrl+W` / `Ctrl+U` / `Ctrl+K` deletes.

**PR tab** (read-only)

| Key | Action |
| --- | --- |
| `j` `k` | Move through description and comments |
| `PageUp` `PageDown` | Scroll focused pane |
| `o` | Open PR in browser |
| `y` | Copy PR URL |
| `r` | Refresh |

**Changes in SCM mode** (`x`)

| Key | Action |
| --- | --- |
| `j` `k` | Move — the file under the cursor opens its diff in the read pane |
| `Enter` | Stage or unstage the file; collapse a section or drawer header |
| `a` `U` | Stage all / unstage all |
| `m` | Preview markdown file; `PageUp` `PageDown` scroll it |
| `.` | Context menu for the row (discard, copy path, checkout, stash, tag…) |
| `c` | Focus the commit box (once something is staged) — on an empty box, also generate a message with ✧ |
| `Enter` (commit box) | Commit the staged files |
| `Shift+Enter` `Alt+Enter` `Ctrl+J` (commit box) | New line: a subject, a blank line, and a body commit as written |
| `Esc` (commit box) | Back to the list, keeping the draft |
| `y` `n` | Answer a destructive action's confirm |
| `/` | Search files and code |
| `r` | Re-read `git status` and the open drawers |
| `x` | Back to the file tree, restoring the scope you left |

The footer names the keys for whatever is under the cursor — `enter stage` on a changed file,
`enter collapse` on a section, `enter run` in the context menu — and the wheel scrolls the
panel without moving the cursor.

**Git ops tab**

| Key | Action |
| --- | --- |
| `j` `k` | Move through the five operations |
| `Enter` | Run it — merge and rebase pick a branch first |
| `y` `n` | Answer the confirm, which names the branch |
| `r` | Re-read the branch and its ahead/behind |

The mouse works too. Drag over any text to select and copy it, double-click a word,
triple-click a line. Click or drag the line-number gutter to comment. Click files, tabs, and
links, and scroll with the wheel. The **Git ops** tab is keyboard-only.

## The four tabs

- **Changes** — the active scope's changed files with `+/-` stats and totals in the header.
  `x` flips its navigator into SCM mode (below) and back.
- **All files** — any file's current content from the whole worktree, comments too. Ignored
  paths show dimmed.
- **PR** — a read-only mirror of the branch's pull request (GitHub) or merge request
  (GitLab): state, checks, description, and comments, rendered as markdown. hq-deck never writes
  to the forge.
- **SCM mode** (on **Changes**) — `git status` as a panel: **Staged** and **Changes** sections over seven read-only
  drawers (graph, commits, branches, remotes, stashes, tags, worktrees). Rows show the file
  name, with its folder beside it only when two files in a section share a name. Moving the
  cursor onto one opens its `uncommitted` diff in the read pane — entering SCM mode moves the
  scope there, and leaving it puts the scope back — so a comment you start there is an ordinary diff comment, and
  staging the file changes neither the diff nor the comment. The commit box sits at the top;
  `.` opens the row's context menu, where every destructive entry asks first.
- **Git ops** — six operations, each behind a confirm: **Update** (`pull --rebase=false
  --autostash --prune` — a merge pull whatever your `pull.rebase` says, dropping remote
  branches that are gone, as IntelliJ's Update does; rebasing is its own row),
  **Update main branch** (the branch `origin/HEAD` names, else `main`/`master`: a fast-forward-only
  `fetch origin <b>:<b>` from any other branch, a plain Update when it is checked out),
  **Push** (`-u origin HEAD` when the branch has no upstream), **Fetch all**
  (`--all --prune`), **Merge into current…** and **Rebase onto…** (a branch picker, then a
  confirm naming both branches). They run in the background — the pane keeps painting — and a
  failure shows git's whole `stderr` rather than a summary. Conflicts land in SCM mode with the
  usual conflict marker.

## Diff scopes

- **uncommitted** — the working tree vs `HEAD` (staged, unstaged, and untracked).
- **branch** — the working tree vs the merge-base with the base branch: **uncommitted** plus
  the branch's commits. The base is your repo's default branch until you pick another with
  `B` ([Base branch](#base-branch)).
- **last turn** - everything that changed in this worktree since your last mark.
  `M` snapshots the whole worktree (tracked and untracked, through a temporary index) into a private ref; the mark survives restarts and is shared by every hq-deck on the worktree.
- **commits** — one commit, or several in a row, picked with `G`. Read what the agent
  committed one step at a time, without its unsaved edits mixed in.

hq-deck starts in **uncommitted**. `default_scope` changes that. Switching with `u`/`b`/`t`/`g`
wins for the rest of the session. `g` without a pick opens the picker.

Every scope respects `.gitignore`, so build output never clutters **Changes**. To review a file,
track it. **All files** still browses any ignored path. Scopes are a file-tree idea: SCM mode
always shows the working tree as `git status` sees it.

## Configuration

CLI flags:

| Flag | Default | Meaning |
| --- | --- | --- |
| `--poll <ms>` | `2000` | worktree poll interval (min `200`) |
| `--base <ref>` | auto | base for `branch` scope, any rev, overrides the pick |
| `--theme <name>` | `catppuccin` | UI + syntax theme (see below) |
| `--wrap <on\|off>` | `on` | soft-wrap long diff lines (`w` toggles at runtime) |
| `--print-config` | | print the resolved config as JSON and exit |

Everything else lives in the config file:

```text
$HQ_DECK_CONFIG_DIR/config.toml      # when set
~/.config/hq-deck/config.toml        # otherwise ($XDG_CONFIG_HOME respected)
```

Create it if missing.
hq-deck re-reads it on every refresh, so edits apply without a relaunch.

The whole schema, with every key at its default:

```toml
theme = "catppuccin"            # any name from Theme below
default_scope = "uncommitted"   # uncommitted | branch | last-turn
navigator_position = "right"    # right | bottom | left | top
github_host = "github.com"      # one self-hosted GitHub, bare hostname
gitlab_host = "gitlab.com"      # one self-hosted GitLab, bare hostname
# editor = "code -g {file}:{line}"   # unset: the built-in editor

[keybindings]                   # action = [key, …]; unlisted actions keep their defaults
comment = ["c"]

[commit_message]                # the ✧ generator in the SCM commit box
model = "haiku"                 # claude -p --model <model>
extra_args = ["--strict-mcp-config"]
max_diff_bytes = 16384          # the staged diff is truncated to this before it is piped in
timeout_secs = 60               # the generation is killed after this
instructions = "Write a git commit message for the diff on stdin: …"
# instructions_file = "prompts/commit.md"   # instead of `instructions`, never both
# branch_pattern = '(?P<ticket>[A-Z]+-\d+)'  # with `template`: a message shaped by the branch
# template = "{ticket} {summary}"
```

A missing file or omitted key uses its default. An invalid file is rejected whole — the pane
shows the error and recovers on the next refresh after you fix it.

### Commit message generator

The ✧ in the commit box runs `claude -p --model <model> <extra_args…> <instructions>` with the
staged diff on stdin, off the frame loop. It only writes into an **empty** commit box, and only
if the box is still empty and still focused when the answer arrives — whatever you typed in the
meantime wins. Any failure (no `claude`, a timeout, a non-zero exit) lands a filename summary
instead, so the box is never left empty and the pane never blocks.

A repository can override the section for itself in **`.hq-deck.toml`** at its root:

```toml
[commit_message]
model = "sonnet"
instructions_file = ".github/commit-style.md"
```

Only the keys present there change; the rest come from the global `[commit_message]`, and
`[commit_message]` is the only section this file may contain. A relative `instructions_file`
resolves against the repo root here, and against the directory holding `config.toml` in the
global file. The per-repo file is read when you ask for a message, so a typo in it reports at
that moment instead of blocking the rest of the pane.

#### Branch template

`branch_pattern` and `template` shape the message by the branch it is written on.
The pattern is a regex searched in the current branch name.
When it matches, the message is `template` with `{summary}` replaced by the generated line and
`{name}` by the named group `name`; `{name|lower}` lowercases it.
When it does not match, or `HEAD` is detached, the message is the generated line alone.

```toml
[commit_message]
branch_pattern = '(?P<ticket>IDC-\d+)-(?P<scope>.+)'
template = "feat({scope|lower}): {ticket} {summary}\n\nTicket: https://jira.example.com/browse/{ticket}"
```

On `PN-IDC-29308-AI` that gives:

```
feat(ai): IDC-29308 Add retry to the upload client

Ticket: https://jira.example.com/browse/IDC-29308
```

The model writes only the summary; the prefix and the ticket come from the branch, so they are
right even when the fallback runs. The two keys come together, and every placeholder must name
`summary` or a group of the pattern; anything else is a config error at load.

### Theme

One theme colors the whole UI, chrome and syntax together:

```toml
theme = "tokyo-night"
```

`--theme` overrides the file. Match your terminal's light or dark background. Available:

- **Dark:** `catppuccin`, `catppuccin-frappe`, `catppuccin-macchiato`, `dracula`, `nord`,
  `gruvbox`, `one-dark`, `solarized`, `monokai`, `tokyo-night`, `rose-pine`.
- **Light:** `catppuccin-latte`, `gruvbox-light`, `one-light`, `solarized-light`,
  `github-light`, `tokyo-night-day`, `rose-pine-dawn`.

### Navigator position

The navigator starts on the right. Set `navigator_position` to `right`, `bottom`, `left`, or
`top`, or press `p` to cycle clockwise:

```toml
navigator_position = "bottom"
```

`<` grows, `>` shrinks, or drag the divider. `z` hides the navigator altogether and brings it
back.

### Base branch

The **branch** scope diffs against the merge-base with your repo's default branch, the one
`origin/HEAD` names. The header shows the resolved base, `vs main`.

When the trunk is something else, or you review a stacked branch, press `B` (or click the
base name) and pick the branch. The pick is stored for this worktree and holds until you
pick again. Other worktrees on the same clone keep their own pick. Choosing the default
branch records that name.

You can also type any revision, like `HEAD~2`, a tag, or a SHA prefix. The header shows what
resolved: `vs HEAD~2 (a1b2c3d)`.

`--base <ref>` sets the base for this pane. It wins over the pick and disables the picker.

### Editor

`e` opens the file at the line you're on, or the navigator's selected file. On a line you have
already commented, `e` edits the comment instead.

With no `editor` key in the config, `e` opens the built-in editor over the read pane, on any
tab with files. From a diff it edits the worktree file at the same line, and `esc` brings the
diff back. The keys are micro's, and every letter is text:

| Key | Action |
|-----|--------|
| arrows, `home`/`end`, `pgup`/`pgdn` | Move (`ctrl+←/→` by word, `ctrl+home/end` to the ends) |
| `shift` + a move, or a mouse drag | Select |
| `ctrl+s` | Save |
| `ctrl+z` / `ctrl+y` | Undo / redo |
| `ctrl+c` / `ctrl+x` / `ctrl+v` | Copy / cut / paste (the line, with nothing selected) |
| `ctrl+a`, `ctrl+d`, `ctrl+k` | Select all, duplicate the line, cut the line |
| `tab` / `shift+tab` | Indent / dedent, in the file's own indent unit |
| `esc` | Close, asking first about unsaved text |

`ctrl+s` is the only write. If the agent changed the file since you opened it, the save asks:
`o` overwrites, `r` reloads the disk's version, `esc` keeps editing. The title says
`changed on disk` as soon as the poll notices, before you save. A file that is binary, not
UTF-8, too large for the read pane, or mixes `\n` and `\r\n` line endings does not open, and the
footer says why.

Set `editor` to use your own editor instead. hq-deck does not read `$EDITOR` or `$VISUAL`, so the
key behaves the same however the pane was started. A bare command opens at the right line: it
knows vim, neovim, helix, emacs, nano, micro, VS Code and its forks, Zed, Sublime Text,
JetBrains, and the rest of the usual set.

```toml
editor = "micro"
```

A terminal editor takes the pane, and hq-deck refreshes when you quit it. A window editor opens
its own window, so the diff stays up and your save turns up in it on the next poll.

Write the whole command when you need to. `{file}` and `{line}` are hq-deck's, everything else
is your editor's:

```toml
editor = "code -g {file}:{line}"
```

### Keybindings

`[keybindings]` maps an action name to an array of keys. The array replaces that action's
defaults, actions you don't mention keep theirs, and hints show the first key:

```toml
[keybindings]
comment = ["c", "ㅊ"]
select  = ["v", "ㅍ"]
```

Several keys per action serves CJK input sources — bind the character your layout produces
on the same physical key.

The action names and their defaults:

| Action | Default |
| --- | --- |
| `down` / `up` | `j` / `k` |
| `next-hunk` / `prev-hunk` | `]` / `[` |
| `next-file` / `prev-file` | `f` / `F` |
| `scope-uncommitted` / `scope-branch` / `scope-last-turn` / `scope-commits` | `u` / `b` / `t` / `g` |
| `base-pick` / `commit-pick` | `B` / `G` |
| `tab-changes` / `tab-all-files` / `tab-pr` / `tab-git-ops` | `1` / `2` / `3` / `4` |
| `scm-toggle` / `scm-stage-all` / `scm-unstage-all` / `scm-menu` | `x` / `a` / `U` / `.` |
| `wrap` | `w` |
| `preview` | `m` |
| `navigator-position` | `p` |
| `navigator-hide` | `z` |
| `navigator-grow` / `navigator-shrink` | `<` / `>` |
| `select` | `v` |
| `comment` | `c` |
| `edit` / `delete` | `e` / `d` |
| `next-comment` / `prev-comment` | `n` / `N` |
| `comments` | `l` |
| `search` | `/` |
| `find` | `ctrl+f` |
| `keys` | `?` |
| `copy-path` | `alt+y` |
| `send-path` | `alt+s` |
| `mark-turn` | `M` |
| `send` | `s` |
| `copy` | `y`, `Y` |
| `open-pr` | `o` |
| `refresh` | `r` |
| `quit` | `q` |

A key is one printable character, or a `ctrl+`/`alt+` chord like `ctrl+f`. `Tab`, `Esc`, and
`Enter` are fixed. Keys still type normally in the comment box.

### Forge repositories and hosts

The PR tab reads `upstream` when you have one, otherwise `origin`. A standard fork clone works
without setup.

GitHub.com and GitLab.com work without configuration. For one self-hosted instance per forge,
set its bare hostname:

```toml
github_host = "github.example.com"
gitlab_host = "git.corp.example"
```

Matching is exact. hq-deck does not infer SSH aliases like `github.com-work` — use a
canonical-host remote or an `insteadOf` rewrite. Authenticate with
`gh auth login --hostname github.example.com` or
`glab auth login --hostname git.corp.example`.

## Limitations

The known constraints:

**Terminal & theme**
- **Truecolor required** — colors are 24-bit RGB with no 256/8-color fallback. Basic terminals
  render wrong colors.
- **Theme must match the terminal** — the pane keeps the terminal's background, and there is no
  auto light/dark detection yet. You match the theme by hand.
- **Add / remove are red / green** — no secondary cue for colorblind users yet.
- **Box-drawing glyphs required**, but no Nerd Font.

**Platform**
- **Linux only** — the fork dropped the macOS and Windows branches.
- **Clipboard export** uses `wl-copy`, `xclip`, or `xsel`. With none installed it says so and keeps your comments.

**Last turn**
- **The mark is manual** - hq-deck does not watch the agent, so the scope shows everything since your last `M`, your own edits included.

**PR tab (GitHub and GitLab)**
- **Read-only** — needs the forge's authenticated CLI (`gh` or `glab`) and a
  recognized `upstream` or `origin`. Without either it tells you what to fix, and the other
  tabs keep working. Other forges are not supported.
- **One repository, never a cross-repository search** — a readable, recognized `upstream` is
  authoritative, otherwise `origin`. Clones that target different parent repositories stay
  separate.
- **Mirrors the branch's *open* PR or MR** — merged or closed shows as history. Each comment
  surface caps at its newest 100 rows, with a `+more` marker naming the forge when there is
  more.

**Review model**
- **Comments are in-memory and single-session** - quitting loses any you haven't copied out.
- **Sending is all-or-nothing** - `s` and `y` deliver the whole set and clear it. A failure leaves everything in place.
- **No line-number rebasing** — a comment stays locatable by its diff snippet, not its line
  number. hq-deck flags a stale comment instead of dropping it.

**SCM and Git ops**
- **Every write is yours** — nothing stages, commits, or reaches a remote without the keystroke
  that asked for it, and the destructive entries ask again.
- **One git operation at a time** — a second **Git ops** run waits for the first to finish.
- **No conflict resolution** — a merge or rebase that conflicts leaves the repo as git left it
  and marks the files in SCM mode. Finish it in your editor or with `git` directly.
- **The drawers are read-only lists** — graph, commits, branches, remotes, stashes, tags, and
  worktrees show their newest 100 entries; acting on one goes through the `.` menu.
- **✧ degrades instead of failing** — with `claude` missing, slow, or erroring, the box gets a
  filename summary (`Update app.rs and 2 more`) to edit rather than an error.

**Budgets**
- Files over 2 MB or 50,000 lines show a "too large" notice. Binary files get no diff.

## Building from source

[Install](#install) already builds from source.
`just ci` runs fmt-check, clippy, the tests, and a release build.

## License

[MIT](LICENSE) — see [NOTICE](NOTICE) for the two upstreams this fork carries. Syntax highlighting comes from [syntect](https://github.com/trishume/syntect)
and [two-face](https://github.com/CosmicHorrorDev/two-face). Most themes' syntax colors come
from two-face's bundled set.

Bundled `.tmTheme` syntax files in `assets/`, each under its own license:

- [Catppuccin Mocha](https://github.com/catppuccin/bat) — MIT.
- [Tokyo Night](https://github.com/folke/tokyonight.nvim) (`tokyo-night`, `tokyo-night-day`) — Apache-2.0.
- [Rosé Pine](https://github.com/rose-pine/tm-theme) (`rose-pine`, `rose-pine-dawn`) — MIT.
