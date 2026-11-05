// hq-deck: draws nothing. A key opens it (`mod.open:hq-deck`), it splits the focused
// pane to the right with hq-deck in it, and closes itself. A key alone cannot do this:
// its shorthand fills only the first argument of `pane.split`, and hq-deck needs `argv`.
// The new pane starts in the focused terminal's folder, so hq-deck opens that worktree.
// argv runs without a shell, on the machine of the focused terminal, under the PATH of
// that machine's hc daemon, which need not hold the install dir. Name it here.
import { View, div } from "gpui-kit";
import { closeOverlay, props, run } from "herdr";

const LAUNCH = 'PATH="$HOME/.cargo/bin:$HOME/.local/bin:$PATH" exec hq-deck';

export default class HqDeck extends View {
  init(_props, cx) {
    run("pane.split", { direction: "right", argv: ["sh", "-c", LAUNCH] });
    cx.timer.after(0, () => closeOverlay(props().overlay));
  }

  render() {
    return div();
  }
}
