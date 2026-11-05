# Developer shortcuts. Installing doesn't need any of this: see README.md.
.PHONY: build release test lint install uninstall clean

build:            ## release binary in target/release (what ~/.local/bin/herdr-feed points to from a checkout)
	cargo build --release

release:          ## release tarball in build/release
	scripts/package-release.sh

test:             ## the binary's tests and the mod's decisions
	cargo test
	node --test test/*.test.js

lint:
	cargo fmt --check
	cargo clippy --release -- -D warnings
	shellcheck -s sh install.sh uninstall.sh scripts/*.sh

install:
	./install.sh

uninstall:
	./uninstall.sh

clean:
	cargo clean
	rm -rf build
