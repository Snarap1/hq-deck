// hq-deck: draws nothing. A key opens it (`mod.open:hq-deck`), it toggles hq-deck in the
// current tab, and closes itself. If the tab holds an hq-deck pane this mod opened, the
// press closes it; otherwise it splits the focused pane to the right with hq-deck in it.
// A key alone cannot do this: its shorthand fills only the first argument of `pane.split`,
// and hq-deck needs `argv`.
// The new pane starts in the focused terminal's folder, so hq-deck opens that worktree.
// argv runs without a shell, on the machine of the focused terminal, under the PATH of
// that machine's hc daemon, which need not hold the install dir. Name it here.
// That argv is also how a later press recognizes the pane: hc keeps a terminal's argv
// for its whole life, so a pane whose terminal was started with exactly ARGV is ours.
import { View, div } from "gpui-kit";
import { closeOverlay, focused, panes, props, run, terminals } from "herdr";

const LAUNCH = 'PATH="$HOME/.cargo/bin:$HOME/.local/bin:$PATH" exec hq-deck';
const ARGV = ["sh", "-c", LAUNCH];

const isDeck = (terminal) =>
  Array.isArray(terminal.argv) &&
  terminal.argv.length === ARGV.length &&
  terminal.argv.every((arg, i) => arg === ARGV[i]);

// The panes in the current tab whose terminal runs hq-deck as this mod launched it.
function deckPanes() {
  const tab = focused()?.tab;
  if (!tab) return [];
  const decks = new Set(terminals().filter(isDeck).map((t) => t.address));
  return panes().filter((p) => p.tab === tab && p.terminal && decks.has(p.terminal));
}

export default class HqDeck extends View {
  init(_props, cx) {
    const open = deckPanes();
    if (open.length > 0) {
      for (const p of open) run("pane.close", { pane: p.pane });
    } else {
      run("pane.split", { direction: "right", argv: ARGV });
    }
    cx.timer.after(0, () => closeOverlay(props().overlay));
  }

  render() {
    return div();
  }
}
