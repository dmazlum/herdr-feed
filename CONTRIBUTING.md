# Contributing

Thanks for helping. Issues and pull requests are welcome; for anything big,
open an issue first so we can agree on the shape.

## Setup

You need a Mac with Apple silicon, the Herdr desktop app, a coding agent, a
recent stable Rust (edition 2024), Node 20+ for the mod's tests, and `ffmpeg`
and `yt-dlp` for media.

```sh
make build        # target/release/herdr-feed
./install.sh      # links ~/.local/bin/herdr-feed to it, installs the mod and its keys
make test lint
herdr-feed pane   # run the pane by hand in any kitty-graphics terminal
```

After `make build`, the next feed the mod opens runs the new binary. After a
change under `mods/`, run `scripts/install-mod.sh`: the app reloads the mod as
soon as its files change.

## Where things are

| Path | What |
|---|---|
| `src/main.rs` | CLI, config, cache, the batch order, the pane's state file |
| `src/fetch.rs` | Sources, feed parsing, `og:image`, the tip repos, clip prefetch |
| `src/pane.rs` | The pane: cards, keys, images, video through ffmpeg |
| `src/layout.rs` | Sizing the feed's split in the app's layout document |
| `mods/herdr-feed/` | The Herdr app mod: the switch window and when to open, pause and close |
| `mods/herdr-feed/lib/core.js` | The mod's decisions, free of Herdr calls (tested in `test/`) |
| `scripts/` | Mod install and release packaging |

## Adding a source

Keep it free, public and without an account. Reddit and YouTube rate-limit
hard: batch requests, as the existing sources do.

## Style

- Rust: `cargo fmt`, `cargo clippy -- -D warnings`, comments that say why.
- JavaScript: the mod follows `~/.config/herdr-app/AGENTS.md`; keep decisions in
  `lib/core.js` and cover them in `test/core.test.js`.
- Shell: POSIX `sh`, passes `shellcheck -s sh`.
- Commit messages: imperative, short subject line.
