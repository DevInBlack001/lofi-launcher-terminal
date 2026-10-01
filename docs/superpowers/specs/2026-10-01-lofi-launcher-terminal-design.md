# lofi-launcher-terminal: design spec

Date: 2026-10-01
Status: approved, pending implementation plan

## Purpose

When the user opens any terminal emulator or TTY session, a lofi background track starts playing automatically and keeps playing until the last such session closes. The user can switch the mood (and therefore the playlist/source) at any time from the terminal, and optionally from a small TUI. Built in Rust. Must work on any terminal emulator and any TTY login, not just one specific terminal, and must run on any Linux distribution (BSD is explicitly out of scope).

## Non-goals

- No GUI application, no YouTube account integration, no playlist editing UI (editing is done by hand in a config file).
- No attempt to support BSD or non-Linux platforms.
- No bundling or redistribution of third-party media. The tool only points a player at user-supplied sources.

## Architecture

Two binaries from one Cargo workspace:

- `lofi-daemon`: a long-lived background process that owns exactly one `mpv` child process and exposes a small IPC protocol over a Unix socket.
- `lofi`: a thin CLI client that talks to the daemon's socket. Also hosts an optional `lofi tui` subcommand (ratatui) that reuses the same client calls.

### Daemon

- Spawns `mpv --idle --no-video --input-ipc-server=$XDG_RUNTIME_DIR/lofi-mpv.sock` once per daemon lifetime.
- Listens on `$XDG_RUNTIME_DIR/lofi-daemon.sock` for newline-delimited JSON commands: `register`, `unregister`, `mood <name>`, `next`, `pause`, `resume`, `status`, `moods`.
- Keeps an in-memory session refcount. `register` increments it; if it was 0, playback of the current mood's sources starts. `unregister` decrements it; at 0, playback stops but the daemon and mpv process stay resident (idle) so the next `register` resumes instantly.
- Self-spawns: the CLI checks for the daemon socket, and if absent, launches the daemon as a detached background process before sending the command. No systemd unit is required, though a user systemd unit remains an easy manual alternative for anyone who wants one.
- Reads `~/.config/lofi-launcher/config.toml` on startup and on `SIGHUP`/explicit `reload` command.

### CLI client (`lofi`)

- `lofi register` / `lofi unregister`: called from shell integration hooks.
- `lofi mood <name>`: switches the active mood and restarts playback from that mood's source list.
- `lofi next`: skips to the next source within the current mood's list.
- `lofi pause` / `lofi resume`: pause/resume without dropping the session count.
- `lofi status`: prints current mood, playing/paused, current source.
- `lofi moods`: lists configured mood names.
- `lofi tui`: opens a ratatui screen showing current mood/track with arrow-key mood selection and pause/skip, calling the same socket commands as the CLI subcommands.

### Config file

`~/.config/lofi-launcher/config.toml`. Each mood maps to a list of sources, where a source is either a local file/directory path or any URL mpv's built-in ytdl hook can resolve (YouTube, internet radio streams, etc.). The user fills in or edits sources themselves; nothing is hardcoded or scraped by this tool.

```toml
default_mood = "code-and-chill"

# Each mood is a list of sources.
# A source is either a local path or a URL mpv/yt-dlp can resolve.
# Synthwave, retrowave, and vaporwave are genre flavors, not separate moods:
# mix them into whichever mood's list fits (e.g. vaporwave under chill-beats).
[moods.code-and-chill]
sources = []

[moods.deep-focus]
sources = []

[moods.chill-beats]
sources = []

[moods.rainy-day]
sources = []
```

If the active mood has no sources configured, the CLI and daemon report a clear warning instead of failing silently or crashing.

Synthwave, retrowave, and vaporwave are not separate top-level moods. They are genre flavors a user can mix into any of the four moods' source lists (for example, a vaporwave playlist fits well under `chill-beats` or `rainy-day`, a driving synthwave mix fits under `code-and-chill`). The config format already supports this since `sources` is just a list, so no schema change is needed, only documentation in the shipped config comments pointing this out.

### Shell integration (any terminal, any TTY)

A shell snippet is installed into the user's shell rc file (bash or zsh, detected at install time) that runs:

```sh
lofi register
trap 'lofi unregister' EXIT
```

Because this runs at interactive-shell startup, it applies uniformly to every terminal emulator that launches a shell (foot, alacritty, kitty, ghostty, xterm, and so on) and to raw TTY logins, with no per-terminal-emulator configuration needed.

### Dependency and portability checks

- At daemon startup, check for `mpv` on `PATH` (with an environment variable override for a non-standard install location, per this user's standing preference against hardcoded paths). If missing, print "mpv not detected, install it via your distro's package manager" and exit without crashing other functionality.
- No distro-specific assumptions beyond "Linux with XDG_RUNTIME_DIR and a POSIX shell." BSD is explicitly unsupported and not tested against.

## Packaging and lifecycle scripts

### `scripts/install.sh`

- Builds release binaries (`cargo build --release`) if not already built.
- Installs `lofi` and `lofi-daemon` to `~/.local/bin` (or `$PREFIX/bin` if `PREFIX` is set, falling back gracefully, never hardcoding a system path).
- Creates `~/.config/lofi-launcher/config.toml` from a template if one does not already exist (never overwrites an existing config).
- Detects the user's shell (`$SHELL`) and appends the register/trap snippet to the matching rc file (`.bashrc` or `.zshrc`), guarded by a marker comment so re-running install is idempotent.
- Checks for `mpv` and warns (does not fail) if missing.

### `scripts/update.sh`

- Pulls latest source (if run inside a git checkout) or assumes the user re-ran install over a new source tree.
- Rebuilds and reinstalls binaries in place.
- Leaves the user's existing config and shell rc snippet untouched.

### `scripts/uninstall.sh`

- Removes the installed binaries.
- Removes the shell rc snippet (only the marker-guarded block it added).
- Leaves the user's config file in place by default, with a prompt (or `--purge` flag) to remove it entirely.
- Stops any running `lofi-daemon` process for the current user before removing files.

### `PKGBUILD`

Since the tool is distro-portable but the user also wants first-class Arch support, a `PKGBUILD` is added and tracked in git at the repo root (or `packaging/PKGBUILD`), building from source via `cargo build --release`, declaring `mpv` as a runtime dependency and `rust`/`cargo` as a build dependency, and installing the two binaries plus the default config template and shell integration snippet via the package's `package()` function. This is one packaging option among others (manual install script, future distro packages); it does not replace `scripts/install.sh`, which remains the generic non-Arch path.

## Error handling

- Daemon dying mid-session (crash, killed) is recovered transparently: the next `lofi` command detects the dead socket and respawns the daemon.
- Missing `mpv`/`yt-dlp` binary: clear, actionable message, no silent failure, no crash of unrelated commands (e.g. `lofi status` still reports "daemon not running, mpv missing" rather than panicking).
- Per this user's standing preference: never hardcode filesystem paths that vary by install. Config path respects `XDG_CONFIG_HOME` with a fallback to `~/.config`; binary install path respects `PREFIX` with a fallback to `~/.local/bin`.

## Testing

- Unit tests: IPC command parsing, refcount transitions (register/unregister sequences), config file parsing including the "mood with no sources" warning path.
- Manual end-to-end test: open and close real terminal windows (at least two different emulators) and a TTY session, confirming the daemon starts on first `register`, stays alive across overlapping sessions, and stops playback only when the count reaches zero. Confirm mood switching and `lofi tui` against a real mpv instance with at least one configured source.

## Out of scope for v1

- BSD support.
- GUI playlist editor.
- Automatic playlist discovery or recommendation.
