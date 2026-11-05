#!/bin/sh
# Build herdr-feed (or use the prebuilt binary next to this script), link it into
# ~/.local/bin and put the Herdr desktop app mod and its keys in place through
# scripts/install-mod.sh. Then fill the cache once.
set -e
cd "$(dirname "$0")"

if [ -f Cargo.toml ]; then
  cargo build --release -q
  bin="$PWD/target/release/herdr-feed"
else
  bin="$PWD/herdr-feed"   # release tarball
fi
mkdir -p ~/.local/bin && ln -sf "$bin" ~/.local/bin/herdr-feed

for t in ffmpeg yt-dlp; do
  command -v "$t" >/dev/null || echo "herdr-feed: $t not found; images and videos need it (brew install $t)"
done

scripts/install-mod.sh
~/.local/bin/herdr-feed fetch
echo "herdr-feed installed: press cmd-shift-e in Herdr to show the switch"
