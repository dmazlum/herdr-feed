#!/bin/sh
# Build the release tarball: the herdr-feed binary, install/uninstall scripts and
# the Herdr mod with its keys.
# Usage: scripts/package-release.sh [vX.Y.Z]   (prints the tarball path)
set -e
cd "$(dirname "$0")/.."
version=${1:-v$(sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -1)}
target=aarch64-apple-darwin
cargo build --release --locked --target "$target" -q
name="herdr-feed-$version-$target"
out=build/release
rm -rf "${out:?}/${name:?}" && mkdir -p "$out/$name"
cp "target/$target/release/herdr-feed" install.sh uninstall.sh keys.json README.md LICENSE "$out/$name/"
mkdir -p "$out/$name/mods/herdr-feed/lib" "$out/$name/scripts"
cp mods/herdr-feed/main.js mods/herdr-feed/mod.json "$out/$name/mods/herdr-feed/"
cp mods/herdr-feed/lib/core.js "$out/$name/mods/herdr-feed/lib/"
cp scripts/install-mod.sh "$out/$name/scripts/"
tar -C "$out" -czf "$out/$name.tar.gz" "$name"
echo "$out/$name.tar.gz"
