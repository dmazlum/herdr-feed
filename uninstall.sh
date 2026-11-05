#!/bin/sh
# Remove herdr-feed's Herdr mod, its keys and the binary link. --purge also
# deletes the cache (~/.cache/herdr-feed) and config (~/.config/herdr-feed).
set -e
cd "$(dirname "$0")"
purge=
for a in "$@"; do
  case "$a" in
    --purge) purge=1 ;;
    *) echo "usage: uninstall.sh [--purge]" >&2; exit 2 ;;
  esac
done

scripts/install-mod.sh --remove
rm -f ~/.local/bin/herdr-feed

if [ -n "$purge" ]; then
  rm -rf ~/.cache/herdr-feed ~/.config/herdr-feed
  echo "cache and config removed"
fi
echo "herdr-feed uninstalled"
