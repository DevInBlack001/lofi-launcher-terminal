# lofi-launcher-terminal

Plays mood-based lofi music in the background for as long as any terminal
or TTY session is open on the machine.

## How it works

Opening a terminal or logging into a TTY runs `lofi register`, which starts
a shared background daemon on first use and starts playback through `mpv`.
Closing the session runs `lofi unregister`. Music keeps playing as long as
at least one session is open, and stops when the last one closes. All
terminals and TTYs share the same daemon and the same `mpv` process: opening
a second terminal while one is already playing increments a session count
and reuses the existing playback, it never starts a second track.

## Online vs. offline

Most lofi content people point this at lives on YouTube, so `lofi` is mostly
an online tool: `lofi add` fetches a URL's metadata over the network to
classify it, and playback of a URL source streams over the network through
mpv's ytdl hook. Point a mood's `sources` at local `.mp3`/`.flac`/etc. files
instead, and that mood plays entirely offline, no network access at all for
those sources. Mixing local and URL sources within the same mood is fine.

## Install

```sh
./scripts/install.sh
```

Open a new terminal afterward to pick up the shell integration.

## Update

```sh
./scripts/update.sh
```

## Uninstall

```sh
./scripts/uninstall.sh          # keeps your config
./scripts/uninstall.sh --purge  # also removes your config
```

## Arch Linux

A `PKGBUILD` is included for Arch-based distributions:

```sh
makepkg -si
```

## Configuring moods

Edit `~/.config/lofi-launcher/config.toml`, or use `lofi add` (below). Each
of the five built-in moods (`code-and-chill`, `deep-focus`, `chill-beats`,
`rainy-day`, `ambient`) takes a list of sources, where a source is a local
file/directory path or any URL mpv's built-in ytdl hook can resolve.
Synthwave, retrowave, and vaporwave are genre flavors: mix them into
whichever mood's list fits, they don't get their own mood keys. A long
source (a multi-hour YouTube mix, for example) is never downloaded or cut
into clips; mpv just seeks to a random point in it each time it's selected.

## CLI

```sh
lofi status            # show current mood, playing/paused, current source
lofi mood deep-focus    # switch mood
lofi next               # skip to the next source in the current mood
lofi pause / lofi resume
lofi moods              # list configured mood names
lofi add <url-or-path>  # classify a source into a mood and add it
lofi add <url-or-path> --mood ambient  # add it to a specific mood directly
lofi tui                # interactive mood picker
```

## Requirements

Linux, `mpv` installed and on `PATH` (or pointed to via `LOFI_MPV_BIN`). `yt-dlp` is needed for `lofi add` to classify a URL source and for mpv to stream URL sources at all; local file sources need neither.

## Now-playing widgets (quickshell, playerctl, waybar)

Install `mpv-mpris` (available in most distro repos, or as an optdepend on Arch) so mpv publishes track title and play/pause state over MPRIS. Any standard MPRIS-reading widget then sees and can control the current lofi track. This is optional: playback works the same without it, only widget visibility is affected. If your `mpv-mpris` script lives somewhere nonstandard, point to it with `LOFI_MPV_MPRIS_SCRIPT=/path/to/mpris.so`.
