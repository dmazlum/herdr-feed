# Changelog

All notable changes to herdr-feed. The format follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/).

## [Unreleased]

## [0.1.1] - 2026-11-05

### Added

- Linux support, experimental: release packages for `x86_64-unknown-linux-gnu` and `aarch64-unknown-linux-gnu`, `ctrl-shift-e` / `ctrl-alt-e` keys from `keys.linux.json` (the installer picks the file for the platform), and links opened with `xdg-open`. Not yet tried in the Herdr app on Linux; reports welcome.

## [0.1.0] - 2026-11-05

### Added

- A side pane in the Herdr desktop app that rotates short tech cards (news, tips, product launches, releases, YouTube and Reddit videos) beside a coding agent while it works. Built on [rexfeed](https://github.com/alpcanaydin/rexfeed) 0.1.1 by Alpcan Aydın.
- A Herdr app mod that replaces rexfeed's per-agent hooks. It watches Herdr's own agent status, so it works with every agent Herdr recognizes: the feed opens in a split to the right of the focused agent when it starts working (focus stays on the agent), pauses while you look at it or another tab, and closes when the agent goes idle, done or blocked, or when you move to another agent. Closing it with `q` keeps it shut for the rest of that turn.
- A small switch window (`cmd-shift-e`) that runs the mod and shows its state; click it or press `cmd-alt-e` to turn the feed off and on.
- `herdr-feed resize`, which narrows the feed to `pane_ratio` (38%) of the row by rewriting the two panes' weights in the app's layout document in herdr-core. Versioned writes, a 5 s answer timeout, and a 3 s wait for a fresh split to reach the document.
- `herdr-feed ready` and `herdr-feed state KEY running|paused|closed` for the mod; `herdr-feed pane --driven` leaves the state file to the mod.
- Batches arranged as video, image card, text card (`video_every`, `media_first`); a kind that runs out is filled by the others.
- `video_prefetch` (default 20) clips downloaded ahead per refresh, 8 at a time.
- The key hint on the card spells the keys out as far as the pane's width allows.
- `install.sh`, `uninstall.sh` and `scripts/install-mod.sh` for the binary link, the mod and its keys.

### Changed from rexfeed

- Static images are placed at the default z-index: Herdr's terminal draws negative z below the card's cell background, which hid every image.
- The cache, config and log live under `~/.cache/herdr-feed` and `~/.config/herdr-feed`.

### Fixed from rexfeed

- A fetch no longer deletes another fetch's partial clips; abandoned ones go after 15 minutes.
- A video card drawn while paused keeps its clip: the 12 s no-frames check counts playing time only.
- `og:image` URLs go through the same http(s) check as every other image URL.
- Code blocks in tips keep their indentation, and HTML entities are decoded once.
- A very small pane no longer underflows the layout math.
- The card is redrawn when the pane changes size, even where the host sends the program no resize signal (Herdr's split resize doesn't), instead of keeping a layout for the old width.

[Unreleased]: https://github.com/dmazlum/herdr-feed/compare/v0.1.1...HEAD
[0.1.1]: https://github.com/dmazlum/herdr-feed/compare/v0.1.0...v0.1.1
[0.1.0]: https://github.com/dmazlum/herdr-feed/releases/tag/v0.1.0
