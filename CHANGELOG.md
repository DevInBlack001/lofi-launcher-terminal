# Changelog

All notable changes to this project are documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [0.3.0] - 2026-10-02

### Added

- `lofi remove <mood> <source>` and a `d` key in the TUI's source browser
  remove a source from a mood's list (`d` arms it, Enter confirms, Esc or
  moving to a different source cancels it). Local files are only removed
  from the config, never deleted from disk.

### Fixed

- A bare `Enter` press on a source that had never been armed with `d`
  silently armed it for deletion anyway, so a second ordinary `Enter`
  press (normal browsing, not a deliberate confirm) could delete it
  instead of playing it. `Enter` no longer has any effect on a pending
  deletion unless `d` armed it first.

## [0.2.0] - 2026-10-02

### Added

- A `loop_playback` setting (default on) with automatic advance to the next
  source in a mood when one ends naturally, wrapping after the last; a new
  persistent background thread listens for mpv's own `end-file` event to
  detect this. `lofi loop on`/`lofi loop off` and an `l` key in the TUI
  toggle it; `lofi status`/the TUI report a real `playing`/`paused`/
  `stopped` state instead of only `playing`/`paused`.
- An `audio_quality` setting (`min`, the default, or `max`), applied as an
  audio-only `ytdl-format` selector before every URL load, so this tool
  never fetches a muxed video+audio stream regardless of a source's
  available formats. `lofi quality min`/`lofi quality max` and a toggle key
  in the TUI switch it live.
- Long sources that are actually YouTube live streams (detected via mpv's
  own live/seekability properties) are no longer treated as "long" for the
  random-seek feature, since seeking into a live broadcast doesn't make
  sense; they now just play continuously from the live edge.
- Browsable chapters in the TUI: a mood's sources can be drilled into, and
  a URL source with YouTube's own chapter markers can be drilled into
  further and jumped to directly, without downloading or re-seeking past
  the chosen point.
- Full playlist support for `lofi add`: a playlist URL is no longer forced
  to a single video. Each entry is classified independently into its own
  best-matching mood (not one shared mood for the whole playlist), with a
  summary of how many entries landed in each mood. A link copied while a
  YouTube "Mix"/"Radio" autoplay was running (`list=RD...`) is treated as
  its single video, not the whole auto-generated mix; a real user-curated
  playlist (`list=PL...` etc) is unaffected.
- `lofi add` can optionally download a source's audio locally instead of
  streaming it: `--download`/`--no-download` flags, or an interactive
  `[y/N]` prompt when neither is given and the terminal is interactive
  (defaults to not downloading when run non-interactively). Downloads are
  audio-only, saved under `$XDG_DATA_HOME/lofi-launcher/<mood>/` (falling
  back to `$HOME/.local/share/lofi-launcher/<mood>/`), and the resulting
  local file, not the original URL, is what gets added as the source. This
  works for both single videos and whole playlists, each entry downloaded
  into its own classified mood's directory.
- `lofi reload` re-reads `config.toml` from disk, so hand edits made while
  the daemon is running can take effect without restarting it.
- `scripts/install.sh` now offers to install missing `mpv`/`yt-dlp` via the
  detected package manager (`pacman`, `apt-get`, `dnf`, `zypper`, or `apk`),
  showing the exact `sudo` command and asking for confirmation first; it
  never runs a privileged command without asking, and in a non-interactive
  run it only prints the command.
- `version.json` is now the single source of truth for the project's
  version; `./scripts/sync-version.sh` propagates it to `Cargo.toml` (via
  `version.workspace = true`) and `PKGBUILD`.

### Changed

- `lofi add`'s classifier now scores every mood by weighted keyword
  occurrence count (title weighted higher than description) instead of
  picking the first alphabetical mood with any match, and gives a strong
  bonus to a bracketed genre tag in the title (e.g. `[synthwave]`), so an
  explicit, deliberate genre label isn't outscored by a generic word
  appearing more often elsewhere. Added `"vaporwave"` to `chill-beats`'s
  keywords and `"retrowave"` alongside `"synthwave"` in `code-and-chill`'s.
- `lofi add` now re-reads `config.toml` from disk before appending a
  source and writes back with a comment-preserving editor, so it no longer
  silently overwrites hand edits made while the daemon was running, or
  strips the file's comments.
- `lofi <command>` now exits non-zero when the daemon returns an error
  response, so scripts checking `$?` can detect failures.
- Only one `lofi-daemon` can run per user at a time (an advisory file lock
  under `$XDG_RUNTIME_DIR`); a second one started while one is already
  running prints a message and exits cleanly instead of racing the first
  for the socket and orphaning its `mpv` process.
- mpv is now spawned with `--keep-open=no --loop-playlist=no`, so a
  setting in the user's own `mpv.conf` can no longer silently prevent the
  events the auto-advance feature depends on.
- The unmaintained `fs2` crate was replaced with the standard library's
  `std::fs::File::try_lock` (stable since Rust 1.89) for the single-
  instance daemon lock.

### Fixed

- A race window in the long-source random-seek logic that could let a
  stale seek silently overwrite an explicit chapter jump or a `next` has
  been closed.
- `install.sh`/`uninstall.sh` now validate that `XDG_CONFIG_HOME` is an
  absolute path (falling back to `$HOME/.config` otherwise), matching the
  daemon's own validation; `update.sh`'s post-restart safety check no
  longer blindly waits a full second.
- A config file that exists but is unreadable due to permissions now
  produces a clear error instead of silently falling back to defaults,
  distinguishing that case from a genuinely missing config file.

### Known limitations

- The classifier can still tie on a title containing both a multi-word
  phrase keyword and a shorter keyword as a substring (e.g. "study" inside
  "Studying"), in which case the mood with the alphabetically earlier name
  wins; a full fix needs word-boundary-aware matching, a larger change to
  the scoring algorithm than this release makes.
- Comment-based tracklists (a timestamped tracklist posted in a video's
  comments rather than its description) aren't picked up as chapters,
  since YouTube only recognizes description-based timestamps as official
  chapters; fetching and parsing comments is a larger, more fragile
  feature left for a future release.

## [0.1.1] - 2026-10-02

### Fixed

- `scripts/install.sh`'s installed shell snippet now uses the absolute path
  to the `lofi` binary instead of a bare `lofi`, since the snippet can end up
  running before any `PATH` additions elsewhere in the rc file.
- `scripts/install.sh` now warns at install time if anything earlier in the
  rc file execs into tmux, screen, or another shell (a bare `tmux` line is
  common), since the installed block can never run for a terminal that
  attaches there, meaning `lofi register` and its `EXIT` trap never fire,
  which can silently leak the session count and leave music playing after
  every tracked terminal is closed. The block must be placed before that
  line by hand, after any "interactive shell only" check, for it to work.

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
