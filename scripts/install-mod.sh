#!/bin/sh
# Put the herdr-feed mod into the Herdr desktop app and merge its keys, or take
# both out again with --remove. The app's home follows HERDR_APP_HOME
# (default ~/.config/herdr-app). keys.json is backed up to
# keys.json.bak-herdr-feed before it changes; running this again changes nothing.
set -e
cd "$(dirname "$0")/.."
home=${HERDR_APP_HOME:-$HOME/.config/herdr-app}
mods="$home/mods"
remove=
case "${1:-}" in
  "") ;;
  --remove) remove=1 ;;
  *) echo "usage: install-mod.sh [--remove]" >&2; exit 2 ;;
esac

if [ -z "$remove" ] && [ ! -d "$home" ]; then
  echo "herdr-feed: no Herdr desktop app at $home (install it from https://herdr.dev first)" >&2
  exit 1
fi

# keys.json: add or drop our bindings, leaving every other one as it is
keys() {
  [ -f "$home/keys.json" ] || [ -z "$remove" ] || return 0
  python3 - "$home/keys.json" "$PWD/keys.json" "$remove" <<'PY'
import json, os, shutil, sys
path, ours_path, remove = sys.argv[1], sys.argv[2], sys.argv[3] == "1"
ours = json.load(open(ours_path))["global"]
cur = json.load(open(path)) if os.path.exists(path) else {}
glob = cur.setdefault("global", {})
before = json.dumps(cur, sort_keys=True)
for k, v in ours.items():
    if remove:
        if glob.get(k) == v:
            del glob[k]
    elif k in glob and glob[k] != v:
        print(f"herdr-feed: {k} is already bound to {glob[k]}; left as it is", file=sys.stderr)
    else:
        glob[k] = v
if json.dumps(cur, sort_keys=True) != before:
    if os.path.exists(path):
        shutil.copy(path, path + ".bak-herdr-feed")
    with open(path, "w") as f:
        json.dump(cur, f, indent=2, ensure_ascii=False)
        f.write("\n")
PY
}

if [ -n "$remove" ]; then
  rm -rf "$mods/herdr-feed"
  keys
  echo "herdr-feed: mod and keys removed"
  exit 0
fi

mkdir -p "$mods/herdr-feed/lib"
cp mods/herdr-feed/main.js mods/herdr-feed/mod.json "$mods/herdr-feed/"
cp mods/herdr-feed/lib/core.js "$mods/herdr-feed/lib/"
keys
if command -v herdr-app >/dev/null; then
  herdr-app check "$mods/herdr-feed"
fi
echo "herdr-feed: mod in $mods/herdr-feed, keys merged"
