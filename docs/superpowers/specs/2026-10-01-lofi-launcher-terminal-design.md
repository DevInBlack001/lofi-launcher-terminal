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

- Spawns `mpv --idle --no-video --input-ipc-server=$XDG_RUNTIME_DIR/lofi-mpv.sock` once per daemon lifetime.
- Listens on `$XDG_RUNTIME_DIR/lofi-daemon.sock` for newline-delimited JSON commands: `register`, `unregister`, `mood <name>`, `next`, `pause`, `resume`, `status`, `moods`.
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
```

An active mood with an empty source list produces a warning from the CLI and daemon, clearly stating the mood has no sources configured.

Synthwave, retrowave, and vaporwave are genre flavors a user can mix into any of the four moods' source lists: a vaporwave playlist fits well under `chill-beats` or `rainy-day`, a driving synthwave mix fits under `code-and-chill`. The config format already supports this since `sources` is just a list, so the shipped config comments document the convention and no schema change is needed.

### Shell integration (any terminal, any TTY)

A shell snippet is installed into the user's shell rc file (bash or zsh, detected at install time) that runs:

```sh
lofi register
trap 'lofi unregister' EXIT
```

This runs at interactive-shell startup, so it applies uniformly to every terminal emulator that launches a shell (foot, alacritty, kitty, ghostty, xterm, and so on) and to raw TTY logins, with a single shared snippet covering all of them.

### Dependency and portability checks

- At daemon startup, check for `mpv` on `PATH`, with an environment variable override available for a non-standard install location. A missing binary produces the message "mpv not detected, install it via your distro's package manager" and the daemon exits cleanly, leaving other functionality unaffected.
- The only platform assumptions are a Linux kernel, `XDG_RUNTIME_DIR`, and a POSIX shell.

## Packaging and lifecycle scripts

### `scripts/install.sh`

- Builds release binaries (`cargo build --release`) when they aren't already built.
- Installs `lofi` and `lofi-daemon` to `~/.local/bin`, or `$PREFIX/bin` when `PREFIX` is set.
- Creates `~/.config/lofi-launcher/config.toml` from a template when one isn't already present, preserving any existing config.
- Detects the user's shell (`$SHELL`) and appends the register/trap snippet to the matching rc file (`.bashrc` or `.zshrc`), guarded by a marker comment so re-running install stays idempotent.
- Checks for `mpv` and prints a warning when it's missing, continuing the rest of the install.

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

A `PKGBUILD` is added and tracked in git at the repo root, for users on Arch-based distributions, building from source via `cargo build --release`, declaring `mpv` as a runtime dependency and `rust`/`cargo` as a build dependency, and installing the two binaries plus the default config template and shell integration snippet via the package's `package()` function. `scripts/install.sh` remains the general path for other distributions.

## Error handling

- A daemon that dies mid-session (crash, killed) is recovered transparently: the next `lofi` command detects the dead socket and respawns the daemon.
- A missing `mpv`/`yt-dlp` binary produces a clear, actionable message; `lofi status` still reports "daemon not running, mpv missing" cleanly.
- Filesystem paths that vary by install resolve through an environment variable first, with a verified fallback: config path respects `XDG_CONFIG_HOME` with a fallback to `~/.config`, and binary install path respects `PREFIX` with a fallback to `~/.local/bin`.

## Testing

- Unit tests: IPC command parsing, refcount transitions (register/unregister sequences), config file parsing including the "mood with no sources" warning path.
- Manual end-to-end test: open and close real terminal windows (at least two different emulators) and a TTY session, confirming the daemon starts on first `register`, stays alive across overlapping sessions, and stops playback only when the count reaches zero. Confirm mood switching and `lofi tui` against a real mpv instance with at least one configured source.

## Future work

- BSD support.
- A GUI playlist editor.
- Automatic playlist discovery or recommendation.
