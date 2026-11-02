#!/usr/bin/env bash
# Installs the hq-deck mod into herdr-app and binds its key.
# Never overwrites a key the user already bound to something else.
set -euo pipefail

src="$(cd "$(dirname "$0")/../integrations/herdr" && pwd)"
home="${HERDR_APP_HOME:-$HOME/.config/herdr-app}"
keys="$home/keys.json"

command -v jq >/dev/null || { echo "install_herdr: jq is required" >&2; exit 1; }
[ -d "$home" ] || { echo "install_herdr: no herdr-app home at $home" >&2; exit 1; }

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
