# lofi-launcher-terminal: design spec

Date: 2026-10-01
Status: approved, pending implementation plan

## Purpose

When the user opens any terminal emulator or TTY session, a lofi background track starts playing automatically and keeps playing until the last such session closes. The user can switch the mood (and therefore the playlist/source) at any time from the terminal, and optionally from a small TUI. Built in Rust. The shell-hook trigger applies uniformly across terminal emulators and TTY logins, and the tool targets Linux distributions generally.

## Scope

- GUI playlist editing happens by hand in the config file.
- The tool points a player at sources the user supplies; it does not bundle or redistribute media.
- The supported platform is Linux.

## Architecture

Two binaries from one Cargo workspace:

- `lofi-daemon`: a long-lived background process that owns exactly one `mpv` child process and exposes a small IPC protocol over a Unix socket.
- `lofi`: a thin CLI client that talks to the daemon's socket. Also hosts an optional `lofi tui` subcommand (ratatui) that reuses the same client calls.

### Daemon

- Spawns `mpv --idle --no-video --input-ipc-server=$XDG_RUNTIME_DIR/lofi-mpv.sock` once per daemon lifetime, with an added `--script=<path to mpv-mpris>` flag when the `mpv-mpris` script is found, so mpv publishes title and play/pause state over MPRIS. This lets any standard MPRIS-reading widget (quickshell's music module, playerctl, waybar modules) show and control the current lofi track with no daemon-side D-Bus code: mpv-mpris does that work once it's loaded.
- The `mpv-mpris` script path resolves the same way as other dependencies: an environment variable override first (`LOFI_MPV_MPRIS_SCRIPT`), then the common real-world install locations (`/usr/share/mpv/scripts/mpris.so`, `/usr/lib/mpv/scripts/mpris.so`, and the Nix/Home Manager-style per-user scripts directory), and a plain "mpv-mpris not detected, now-playing widgets won't see this player" warning (not an error) when none match, since MPRIS visibility is a nice-to-have, not required for playback.
- Listens on `$XDG_RUNTIME_DIR/lofi-daemon.sock` for newline-delimited JSON commands: `register`, `unregister`, `mood <name>`, `next`, `pause`, `resume`, `status`, `moods`, `add <source>`.
- A long source (longer than a configured threshold, default 20 minutes) plays from a random start offset each time it's selected, looping back to the start of the file when playback reaches the end, so a multi-hour mix feels like varied clips without downloading or re-encoding anything.
- Keeps an in-memory session refcount. `register` increments it; a count that was 0 starts playback of the current mood's sources. `unregister` decrements it; a count that reaches 0 stops playback while keeping the daemon and mpv process resident (idle), so the next `register` resumes instantly.
- Self-spawns: the CLI checks for the daemon socket and launches the daemon as a detached background process when it's absent, before sending the command. A user systemd unit remains an easy manual alternative for anyone who wants one.
- Reads `~/.config/lofi-launcher/config.toml` on startup and on `SIGHUP`/explicit `reload` command.

### CLI client (`lofi`)

- `lofi register` / `lofi unregister`: called from shell integration hooks.
- `lofi mood <name>`: switches the active mood and restarts playback from that mood's source list.
- `lofi next`: skips to the next source within the current mood's list.
- `lofi pause` / `lofi resume`: pause/resume while keeping the session count.
- `lofi status`: prints current mood, playing/paused, current source.
- `lofi moods`: lists configured mood names.
- `lofi add <source>`: classifies a source (a local path or a URL) into a mood and appends it to that mood's source list in the config file. See "Adding sources and auto-classification" below.
- `lofi tui`: opens a ratatui screen showing current mood/track with arrow-key mood selection and pause/skip, calling the same socket commands as the CLI subcommands.

### Config file

`~/.config/lofi-launcher/config.toml`. Each mood maps to a list of sources, where a source is a local file/directory path or any URL mpv's built-in ytdl hook can resolve (YouTube, internet radio streams, etc.). The user fills in and edits sources themselves.

```toml
default_mood = "code-and-chill"

# Each mood is a list of sources.
# A source is a local path or a URL mpv/yt-dlp can resolve.
# Synthwave, retrowave, and vaporwave are genre flavors: mix them into
# whichever mood's list fits best (e.g. vaporwave under chill-beats).
[moods.code-and-chill]
sources = []

[moods.deep-focus]
sources = []

[moods.chill-beats]
sources = []

[moods.rainy-day]
sources = []

[moods.ambient]
sources = []

# Keywords used by `lofi add` to classify a new source's title into a mood.
# The first mood whose keyword list matches (case-insensitively) wins.
[classifier]
code-and-chill = ["code", "study", "focus", "synthwave", "productivity"]
deep-focus = ["deep focus", "concentration", "flow state"]
chill-beats = ["chill", "beats", "hip hop", "lofi hip hop"]
rainy-day = ["rain", "storm", "thunder", "cozy"]
ambient = ["ambient", "drone", "atmosphere", "space"]
```

An active mood with an empty source list produces a warning from the CLI and daemon, clearly stating the mood has no sources configured.

Synthwave, retrowave, and vaporwave are genre flavors a user can mix into any of the five moods' source lists: a vaporwave playlist fits well under `chill-beats` or `rainy-day`, a driving synthwave mix fits under `code-and-chill`. The config format already supports this since `sources` is just a list, so the shipped config comments document the convention and no schema change is needed.

### Adding sources and auto-classification

Most lofi content on YouTube is a single long video (a multi-hour mix), not a playlist, so a source is treated as one playable unit regardless of its length; there is no assumption that a source expands into multiple tracks.

`lofi add <source>` resolves which mood a new source belongs in automatically:

1. For a URL, the daemon runs `yt-dlp --dump-json --skip-download <url>` to fetch the title and description as metadata only, without downloading media.
2. The title and description are matched, case-insensitively, against the `[classifier]` keyword lists in the config, in the order the moods are declared. The first match wins.
3. If nothing matches (or the source is a local file with no embedded metadata to match against), `lofi add` reports that it could not classify the source and asks the user to re-run with an explicit mood: `lofi add <source> --mood <name>`.
4. On a successful match or an explicit `--mood` flag, the source is appended to that mood's `sources` list in `~/.config/lofi-launcher/config.toml`, and the daemon is told to reload its config.

A multi-hour source is never downloaded or cut into separate clip files. Instead, when the daemon selects a source whose duration (from `yt-dlp --dump-json`'s `duration` field, cached alongside the source so it isn't re-fetched on every playback) exceeds a configurable threshold (default 20 minutes, set via `long_source_minutes` in the config), it tells mpv to seek to a random timestamp within the file before playing, and to loop back to the start if playback reaches the end of the file. This gives the effect of varied clips from a single long mix with no extra storage, no `ffmpeg` dependency, and no background trimming job.

### Shell integration (any terminal, any TTY)

A shell snippet is installed into the user's shell rc file (bash or zsh, detected at install time) that runs:

```sh
lofi register
trap 'lofi unregister' EXIT
```

This runs at interactive-shell startup, so it applies uniformly to every terminal emulator that launches a shell (foot, alacritty, kitty, ghostty, xterm, and so on) and to raw TTY logins, with a single shared snippet covering all of them.

### Dependency and portability checks

- At daemon startup, check for `mpv` on `PATH`, with an environment variable override available for a non-standard install location. A missing binary produces the message "mpv not detected, install it via your distro's package manager" and the daemon exits cleanly, leaving other functionality unaffected.
- `lofi add` on a URL requires `yt-dlp` on `PATH` (also overridable via an environment variable). A missing `yt-dlp` produces a clear message and `lofi add` falls back to requiring `--mood` to classify a URL, or works normally for local file sources, which don't need `yt-dlp` at all.
- A missing `mpv-mpris` script produces a warning, not an error: playback continues normally through mpv, only now-playing widget visibility is affected.
- The only platform assumptions are a Linux kernel, `XDG_RUNTIME_DIR`, and a POSIX shell.

## Packaging and lifecycle scripts

### `scripts/install.sh`

- Builds release binaries (`cargo build --release`) when they aren't already built.
- Installs `lofi` and `lofi-daemon` to `~/.local/bin`, or `$PREFIX/bin` when `PREFIX` is set.
- Creates `~/.config/lofi-launcher/config.toml` from a template when one isn't already present, preserving any existing config.
- Detects the user's shell (`$SHELL`) and appends the register/trap snippet to the matching rc file (`.bashrc` or `.zshrc`), guarded by a marker comment so re-running install stays idempotent.
- Checks for `mpv` and prints a warning when it's missing, continuing the rest of the install. Also checks for an `mpv-mpris` script in the standard locations and prints a separate, clearly optional warning when it's missing, since it only affects now-playing widget visibility.

### `scripts/update.sh`

- Pulls the latest source when run inside a git checkout, or works against a freshly dropped-in source tree otherwise.
- Rebuilds and reinstalls binaries in place.
- Preserves the user's existing config and shell rc snippet.

### `scripts/uninstall.sh`

- Removes the installed binaries.
- Removes the marker-guarded shell rc snippet block.
- Preserves the user's config file by default; a `--purge` flag removes it too.
- Stops any running `lofi-daemon` process for the current user before removing files.

### `PKGBUILD`

A `PKGBUILD` is added and tracked in git at the repo root, for users on Arch-based distributions, building from source via `cargo build --release`, declaring `mpv` as a runtime dependency, `mpv-mpris` as an optional dependency, and `rust`/`cargo` as a build dependency, and installing the two binaries plus the default config template and shell integration snippet via the package's `package()` function. `scripts/install.sh` remains the general path for other distributions.

## Error handling

- A daemon that dies mid-session (crash, killed) is recovered transparently: the next `lofi` command detects the dead socket and respawns the daemon.
- A missing `mpv`/`yt-dlp` binary produces a clear, actionable message; `lofi status` still reports "daemon not running, mpv missing" cleanly.
- Filesystem paths that vary by install resolve through an environment variable first, with a verified fallback: config path respects `XDG_CONFIG_HOME` with a fallback to `~/.config`, and binary install path respects `PREFIX` with a fallback to `~/.local/bin`.

## Testing

- Unit tests: IPC command parsing, refcount transitions (register/unregister sequences), config file parsing including the "mood with no sources" warning path, keyword classifier matching against sample titles, the long-source random-seek threshold decision.
- Manual end-to-end test: open and close real terminal windows (at least two different emulators) and a TTY session, confirming the daemon starts on first `register`, stays alive across overlapping sessions, and stops playback only when the count reaches zero. Confirm mood switching and `lofi tui` against a real mpv instance with at least one configured source. Confirm `lofi add` against a real multi-hour YouTube lofi mix URL, verifying it classifies into a mood and that playback starts at a random offset.

## Future work

- BSD support.
- A GUI playlist editor.
- Automatic playlist discovery or recommendation.
- Spotify sources, as a second playback backend alongside mpv: this needs a Spotify Premium account, a Spotify Developer app for OAuth, and a Spotify Connect client such as `spotifyd` running as a controllable playback device, since Spotify audio can't be extracted the way mpv/yt-dlp handle YouTube and local files. Scoped out of this version due to that added complexity.
