#!/usr/bin/env bash
# Installs the hq-deck integration into whichever herdr is here: the mod and key for herdr-app,
# the linked plugin for the older herdr.
# Never overwrites a key the user already bound to something else.
set -euo pipefail

root="$(cd "$(dirname "$0")/.." && pwd)"
src="$root/integrations/herdr"
home="${HERDR_APP_HOME:-$HOME/.config/herdr-app}"
keys="$home/keys.json"
herdr_bin="${HERDR_BIN_PATH:-herdr}"

command -v jq >/dev/null || { echo "install_herdr: jq is required" >&2; exit 1; }

have_app=false have_herdr=false
[ -d "$home" ] && have_app=true
command -v "$herdr_bin" >/dev/null && have_herdr=true
if ! $have_app && ! $have_herdr; then
  echo "install_herdr: no herdr-app home at $home and no herdr on PATH" >&2
  exit 1
fi

if $have_herdr; then
  "$herdr_bin" plugin link "$root/integrations/herdr-legacy"
  echo "hq-deck: plugin linked, bind snarap.hq-deck.toggle to a key in your herdr config"
fi

$have_app || exit 0

mkdir -p "$home/mods"
rm -rf "$home/mods/hq-deck"
cp -r "$src/mods/hq-deck" "$home/mods/hq-deck"

[ -f "$keys" ] || echo '{}' > "$keys"
conflicts="$(jq -r --slurpfile new "$src/keys.json" '
  . as $cur | $new[0] | to_entries[] as $s | $s.value | to_entries[]
  | select(($cur[$s.key][.key] // .value) != .value)
  | "\($s.key) \(.key) is bound to \($cur[$s.key][.key])"' "$keys")"
if [ -n "$conflicts" ]; then
  echo "install_herdr: key already taken, edit $src/keys.json or $keys:" >&2
  echo "$conflicts" >&2
  exit 1
fi
tmp="$(mktemp "$keys.XXXXXX")"
jq --slurpfile new "$src/keys.json" '. * $new[0]' "$keys" > "$tmp"
mv "$tmp" "$keys"

herdr-app check "$home/mods/hq-deck" || true
echo "hq-deck: mod installed, super-y opens it to the right"
