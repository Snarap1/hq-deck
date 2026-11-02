// hq-deck: draws nothing. A key opens it (`mod.open:hq-deck`), it splits the focused
// pane to the right with hq-deck in it, and closes itself. A key alone cannot do this:
// its shorthand fills only the first argument of `pane.split`, and hq-deck needs `argv`.
// The new pane starts in the focused terminal's folder, so hq-deck opens that worktree.
import { View, div } from "gpui-kit";
import { closeOverlay, props, run } from "herdr";

export default class HqDeck extends View {
  init(_props, cx) {
    run("pane.split", { direction: "right", argv: ["hq-deck"] });
    cx.timer.after(0, () => closeOverlay(props().overlay));
  }

  render() {
    return div();
  }
}
