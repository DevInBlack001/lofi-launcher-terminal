# Changelog

All notable changes to this project are documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [0.1.0] - 2026-10-02

### Added

- A shared `lofi-daemon` that owns a single `mpv` process and plays music for
  as long as any terminal or TTY session is open, started and stopped via
  shell integration (`lofi register` / `lofi unregister`).
- Five built-in moods (`code-and-chill`, `deep-focus`, `chill-beats`,
  `rainy-day`, `ambient`), each a configurable list of local file or URL
  sources.
- `lofi add <url-or-path>`, which classifies a source into a mood using
  weighted keyword scoring against a URL's fetched title and description
  (via `yt-dlp`), or an explicit `--mood` for sources that can't be
  classified automatically (including all local file paths).
- `lofi mood`, `lofi next`, `lofi pause`, `lofi resume`, `lofi status`,
  `lofi moods`, `lofi reload`, and an interactive `lofi tui` mood picker.
- Long sources (multi-hour mixes) play from a random offset instead of being
  downloaded or cut into clips.
- MPRIS support via `mpv-mpris`, so now-playing widgets (quickshell,
  playerctl, waybar, etc.) can see and control the current track.
- `scripts/install.sh`, `scripts/update.sh`, `scripts/uninstall.sh` for
  general Linux installs, and a `PKGBUILD` for Arch-based distributions.

### Known limitations

- Editing `config.toml` by hand while the daemon is running requires
  `lofi reload` (or a daemon restart) to take effect; the daemon only reads
  the file at startup otherwise.
- `lofi add` always rewrites `config.toml` without preserving comments, since
  it uses a plain TOML serializer rather than a comment-preserving editor.
- The default classifier keywords can still tie on some titles (e.g. a title
  containing both "study" and "deep focus"-adjacent words), in which case the
  mood with the alphabetically earlier name wins the tie.
- `lofi <command>` currently exits `0` even when the daemon returns an error
  response; check the printed output rather than the exit code for now.
- Reload is only available via the explicit `lofi reload` command, not
  automatically on `SIGHUP`, since the signal-handling crate in use can't
  distinguish `SIGHUP` from `SIGTERM`/`SIGINT` in one callback.
