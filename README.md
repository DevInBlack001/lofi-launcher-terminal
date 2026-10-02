# lofi-launcher-terminal

Plays mood-based lofi music in the background for as long as any terminal
or TTY session is open on the machine.

![Adding sources with lofi add](docs/screenshots/cli-add.png)

![Interactive mood picker](docs/screenshots/tui.png)

![lofi help](docs/screenshots/cli-help.png)

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
those sources. `lofi add --download` turns a URL into such a local file
once, so it plays offline from then on. Mixing local and URL sources within
the same mood is fine.

URL sources only ever stream audio: mpv is told to pick an audio-only
stream, so no video data is downloaded just to be discarded. `audio_quality`
in `config.toml` (or `lofi quality min|max`) picks the smallest audio stream
(`min`, the default, easiest on bandwidth) or the best one (`max`). For the
rare source with no audio-only stream (some live streams), the smallest
combined stream is used instead.

## Install

```sh
./scripts/install.sh
```

If `mpv` or `yt-dlp` aren't already installed, the script detects your
package manager (`pacman`, `apt-get`, `dnf`, `zypper`, or `apk`) and offers
to install them, showing the exact `sudo` command first and asking for
confirmation; it never runs a privileged command without asking, and in a
non-interactive run it just prints the command instead of running it.

Open a new terminal afterward to pick up the shell integration.

## Update

```sh
./scripts/update.sh
```

The update stops a running `lofi-daemon` so the new binary takes over; the
new daemon starts on the next `lofi` command. Playback stops until a new
terminal opens, and terminals that were already open are not counted by the
new daemon.

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
source (a multi-hour YouTube mix, for example) is never cut into clips,
and is only downloaded if you ask `lofi add` to (see below); mpv just seeks
to a random point in it each time it's selected.

Live stream URLs (a 24/7 lofi radio on YouTube, for example) work as
sources too, added the same way as any other URL with `lofi add`. They play
continuously from the live edge, without the random seek, since a live
broadcast has no fixed length to seek within. When a live stream ends
because the broadcast goes offline, it is treated like any other source
finishing and moves on to the next one (see below). A source that fails to
load at all, live or not, stops playback instead of moving on, so a dead URL
or a lost connection never turns into an endless retry loop.

A playlist URL adds every video in it, each classified into a mood on its
own title, so one playlist can spread across several moods; a summary at
the end shows how many entries went to each. With `--mood`, that mood is
only used for entries whose title matches no mood; without it, those
entries are skipped and listed with the exact `lofi add <url> --mood <name>`
command to add them one by one. Private or deleted videos are skipped, and
so is anything already in a mood's list, so re-adding a playlist only picks
up its new entries. A watch URL that also carries a `list=` parameter counts
as the playlist; drop the `list=` part to add just that one video.

Instead of streaming, a URL (or every entry of a playlist) can be
downloaded once and then played from disk, offline. Downloads are
audio-only, at the current `audio_quality`, and are stored per mood in
`$XDG_DATA_HOME/lofi-launcher/<mood>/` (`~/.local/share/lofi-launcher/<mood>/`
when `XDG_DATA_HOME` is unset). The mood's list then holds the downloaded
file's path, not the URL. Live streams have no end to download, so they are
always added as streaming sources.

When a source finishes, playback moves on to the next source in the mood,
wrapping back to the first after the last. Set `loop_playback = false` in
`config.toml` (or run `lofi loop off`) to stop at the end of each source
instead.

The daemon reads `config.toml` once at startup, so after editing the file by
hand run `lofi reload` (or restart the daemon) for the changes to take
effect. `lofi add` always re-reads the file before appending, so it never
overwrites hand edits, though it does rewrite the file without its comments.

## CLI

```sh
lofi status            # show current mood, playing/paused/stopped, current source, loop, quality
lofi mood deep-focus    # switch mood
lofi next               # skip to the next source in the current mood
lofi pause / lofi resume
lofi moods              # list configured mood names
lofi reload             # re-read config.toml after editing it by hand
lofi loop on / lofi loop off  # auto-advance to the next source when one finishes (default on)
lofi quality min / lofi quality max  # audio stream quality for URL sources (default min)
lofi add <url>          # classify a URL into a mood and add it (a playlist adds every entry)
lofi add <url-or-path> --mood ambient  # add it to a specific mood directly
lofi add <url> --download     # download the audio and add the local file, without asking
lofi add <url> --no-download  # stream it, without asking
lofi tui                # interactive browser: moods, their sources, and chapters
```

In `lofi tui`, Enter on a mood lists its sources (Space plays the whole mood
right away). Enter on a local file plays it. Enter on a URL looks up its
chapter markers (the timestamped tracklist many mix videos have) and, if it
has any, lists them so Enter can jump straight to one; a URL without chapters
just plays from the start. Space on a source plays it from the start without
looking anything up, and Esc or Backspace goes back up a level. Local files
are never looked up, so browsing an offline mood stays offline. `l` toggles
loop and `a` toggles audio quality.

With neither `--download` nor `--no-download`, `lofi add <url>` asks
`Download this locally instead of streaming? [y/N]` (once for a whole
playlist); Enter or anything but `y`/`yes` streams. When stdin is not a
terminal (a script or pipe) it never asks and streams, so pass `--download`
to download from a script. Both flags are ignored for local paths.

Classification works from a URL's title and description, so `lofi add` on a
local file path always needs `--mood`. Local paths are stored as absolute
paths, so relative paths like `./mix.mp3` work from any directory.

Only one `lofi-daemon` runs per user. Every `lofi` command starts it on
demand, so you normally never run it yourself; if you do start
`lofi-daemon` by hand while one is already running, it prints "another
lofi-daemon is already running" and exits.

## Requirements

Linux, `mpv` installed and on `PATH` (or pointed to via `LOFI_MPV_BIN`). `yt-dlp` is needed for `lofi add` to classify, expand playlists, or download a URL source and for mpv to stream URL sources at all; local file sources need neither.

## Now-playing widgets (quickshell, playerctl, waybar)

Install `mpv-mpris` (available in most distro repos, or as an optdepend on Arch) so mpv publishes track title and play/pause state over MPRIS. Any standard MPRIS-reading widget then sees and can control the current lofi track. This is optional: playback works the same without it, only widget visibility is affected. If your `mpv-mpris` script lives somewhere nonstandard, point to it with `LOFI_MPV_MPRIS_SCRIPT=/path/to/mpris.so`.

## Versioning (for maintainers)

`version.json` is the single source of truth for the project's version. To
bump it, edit `version.json` and run `./scripts/sync-version.sh`, which
propagates the version into `Cargo.toml` (every crate inherits it via
`version.workspace = true`) and `PKGBUILD`, rebuilds `Cargo.lock`, and
regenerates `.SRCINFO`. Review the resulting diff, then commit.
