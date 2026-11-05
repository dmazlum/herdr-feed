# Security policy

herdr-feed adds a mod to the Herdr desktop app, edits the app's layout
document in herdr-core, downloads content from public feeds and plays it in a
terminal pane through `ffmpeg` and `yt-dlp`. Problems in any of that are worth
reporting.

## Reporting a vulnerability

Please don't open a public issue. Report it privately through
[GitHub's security advisories](https://github.com/dmazlum/herdr-feed/security/advisories/new)
with what you found, how to reproduce it and which versions are affected. You'll
get an answer within a few days, and a fix and an advisory when it's confirmed.

## Supported versions

Fixes go into the latest release. Update by installing the new release (or
`git pull && ./install.sh` in a checkout).

## What herdr-feed touches

- `~/.config/herdr-app/mods/herdr-feed/`: the mod. Herdr mods are trusted code
  and run commands; this one runs only `~/.local/bin/herdr-feed`.
- `~/.config/herdr-app/keys.json`: two bindings (`cmd-shift-e`, `cmd-alt-e`; on Linux
  `ctrl-shift-e`, `ctrl-alt-e`).
  The file is backed up to `keys.json.bak-herdr-feed` before it changes.
- herdr-core's `herdr-app` document (the app's pane layout): `herdr-feed resize`
  changes the `size.weight` of the agent's and the feed's panes, with a
  versioned write, and nothing else.
- `~/.local/bin/herdr-feed`, a link to the binary.
- `~/.cache/herdr-feed`: the item cache, the pane's state file, downloaded
  media, cloned tip repositories and a log. `~/.config/herdr-feed/config.json`
  if you create it.
- Feed text and titles are drawn in your terminal; control characters in them
  that reach the terminal are worth reporting.
