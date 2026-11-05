#!/usr/bin/env bash
# The hq-deck pane actions for herdr.
#
#   pane.sh toggle   open a pane, or close every one if any is open
#   pane.sh open     open a pane, no-op if one is open
#   pane.sh close    close every pane, no-op if none
#
# An hq-deck pane is any pane with hq-deck in its foreground process group, read live per pane.
# Actions refuse with exit 1 and one stderr line.
set -uo pipefail

# herdr runs plugin commands with a minimal PATH.
export PATH="/usr/local/bin:/usr/bin:/bin:${PATH:-}"

mode="${1:-toggle}"
H="${HERDR_BIN_PATH:-herdr}"

refuse() {
  printf 'hq-deck: %s\n' "$1" >&2
  exit 1
}

case "$mode" in
toggle | open | close) ;;
*) refuse "unknown mode '$mode' (toggle | open | close)" ;;
esac

ws="${HERDR_WORKSPACE_ID:-}"
pane="${HERDR_PANE_ID:-}"
context="${HERDR_PLUGIN_CONTEXT_JSON:-}"
[ -n "$context" ] || context='{}'
[ -n "$ws" ] || refuse "no workspace context (invoke from inside herdr)"

panes_json=$("$H" pane list --workspace "$ws" 2>/dev/null) &&
  printf '%s' "$panes_json" | jq -e '.result.panes' >/dev/null 2>&1 ||
  refuse "herdr pane list failed for $ws"

# Exit 0 for an hq-deck pane, 1 for any other or one that is gone, 2 for a failed read.
is_deck_pane() {
  if ! info=$("$H" pane process-info --pane "$1" 2>&1); then
    case "$info" in
    *pane_not_found*) return 1 ;;
    *) return 2 ;;
    esac
  fi
  count=$(printf '%s' "$info" | jq -r '
    def base: split("/") | last;
    [.result.process_info.foreground_processes[]
      | select((((.argv0 // "") | base) == "hq-deck")
          or ((((.argv // [])[0] // "") | base) == "hq-deck"))]
    | length' 2>/dev/null) || return 2
  [ "$count" -gt 0 ] 2>/dev/null
}

existing=""
while IFS= read -r p; do
  [ -n "$p" ] || continue
  is_deck_pane "$p"
  case $? in
  0) existing="$existing$p"$'\n' ;;
  2) refuse "herdr pane process-info failed in $ws" ;;
  esac
done < <(printf '%s' "$panes_json" | jq -r '.result.panes[].pane_id // empty')

close_all() {
  while IFS= read -r p; do
    [ -n "$p" ] || continue
    if ! err=$("$H" pane close "$p" 2>&1 >/dev/null); then
      case "$err" in
      *pane_not_found*) ;;
      *) refuse "herdr pane close failed for $p" ;;
      esac
    fi
  done <<<"$existing"
}

if [ -n "$existing" ]; then
  [ "$mode" = open ] || close_all
  exit 0
fi
[ "$mode" != close ] || exit 0

# Open on the focused pane's live directory, else the one the context reports.
is_git_repo() { [ -n "$1" ] && git -C "$1" rev-parse --show-toplevel >/dev/null 2>&1; }
focused=$(printf '%s' "$context" | jq -r '.focused_pane_id // empty' 2>/dev/null)
live=$(printf '%s' "$panes_json" | jq -r --arg p "$focused" \
  'first(.result.panes[] | select(.pane_id == $p) | .foreground_cwd // empty)' 2>/dev/null)
cwd=$(printf '%s' "$context" | jq -r '.focused_pane_cwd // .workspace_cwd // empty' 2>/dev/null)
is_git_repo "$live" && cwd="$live"
is_git_repo "$cwd" || refuse "not a git repo: '${cwd:-<no cwd>}'"

[ -n "$pane" ] || pane="$focused"
[ -n "$pane" ] || pane=$(printf '%s' "$panes_json" | jq -r '.result.panes[0].pane_id // empty')
[ -n "$pane" ] || refuse "no pane to attach to in $ws"

open_json=$("$H" plugin pane open --plugin "${HERDR_PLUGIN_ID:-snarap.hq-deck}" \
  --entrypoint pane --placement split --target-pane "$pane" --direction right \
  --cwd "$cwd" --focus 2>/dev/null)
printf '%s' "$open_json" | jq -e '.result.plugin_pane.pane.pane_id' >/dev/null 2>&1 ||
  refuse "herdr plugin pane open failed"
