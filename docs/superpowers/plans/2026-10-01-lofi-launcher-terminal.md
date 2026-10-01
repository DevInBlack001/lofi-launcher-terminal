# lofi-launcher-terminal Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Build a Rust daemon/CLI pair that starts a shared lofi background track whenever a terminal or TTY opens, keeps it playing while any such session is open, and lets the user switch mood (and therefore playlist) at any time.

**Architecture:** A Cargo workspace with three crates. `lofi-common` holds the config model and the JSON IPC protocol shared by both binaries. `lofi-daemon` owns one `mpv` child process, tracks a session refcount, and serves commands over a Unix socket. `lofi` is the CLI/TUI client; it auto-spawns the daemon on first use and is what shell rc files call on terminal open/close.

**Tech Stack:** Rust (stable), `serde`/`serde_json` for the IPC protocol, `toml`/`serde` for config, `clap` for CLI parsing, `ratatui` + `crossterm` for the TUI, `mpv` (external binary) as the playback engine controlled over its own JSON IPC socket.

**Spec:** `docs/superpowers/specs/2026-10-01-lofi-launcher-terminal-design.md`

## Global Constraints

- No em dashes anywhere: code, comments, docs, commit messages, CLI output strings.
- No `Co-Authored-By` or similar trailers on any commit.
- Code comments only where the WHY is non-obvious; no restating what the code does.
- Never hardcode filesystem paths: config path resolves via `XDG_CONFIG_HOME` falling back to `~/.config`; binary install path resolves via `PREFIX` falling back to `~/.local/bin`; `mpv` binary path resolves via an env var override falling back to `PATH` lookup.
- Runtime sockets live under `XDG_RUNTIME_DIR` (`lofi-daemon.sock`, `lofi-mpv.sock`).
- Supported platform is Linux (POSIX shell, `XDG_RUNTIME_DIR` present). No BSD support attempted.
- Five built-in moods, exact keys: `code-and-chill`, `deep-focus`, `chill-beats`, `rainy-day`, `ambient`. Synthwave/retrowave/vaporwave are documented as genre flavors to mix into these five, never added as separate mood keys.
- A mood with an empty `sources` list must warn clearly, never panic or fail silently.
- A source is treated as a single playable unit regardless of length; a multi-hour YouTube mix is never downloaded or cut into clip files.
- `lofi add` classifies by matching a fetched title/description against the config's `[classifier]` keyword lists; it never performs audio analysis.
- A source whose duration exceeds `long_source_minutes` (default 20) plays from a random start offset and loops at end of file, instead of always starting at time zero.
- `mpv-mpris` is an optional dependency, resolved via `LOFI_MPV_MPRIS_SCRIPT` falling back to a short list of real-world install paths; a missing script produces a warning only, playback continues normally.

## Review Focus

- Config file missing entirely on first run: daemon/CLI must create a default template rather than erroring, since `scripts/install.sh` may not have run first (e.g. a dev build).
- Daemon socket left behind by a crashed daemon (stale socket file, no listener): CLI must detect connect failure, remove the stale socket, and respawn, rather than hanging on connect.
- `register` called many times in quick succession (e.g. several terminal tabs opening at once) or `unregister` called when the count is already 0: refcount must never go negative and must stay consistent under this ordering.
- `mood <name>` given a name not present in the config: CLI/daemon must report an actionable "unknown mood" error listing valid names, not a panic or a silent no-op.
- `mpv` present but a configured source is an unreachable/invalid URL or missing local path: the daemon must surface the mpv error via `status` rather than the process or socket dying, and must stay able to accept the next command (e.g. switch mood away from the bad source).
- `lofi add` on a source whose title/description matches no `[classifier]` keyword list: the CLI must report that it could not classify and ask for an explicit `--mood`, rather than guessing or silently dropping the source.
- `lofi add` on a URL when `yt-dlp` is missing or the URL is a local file path: the CLI must still work for local sources (no metadata to fetch, `--mood` required) and give a clear "yt-dlp not detected" message for URL sources, rather than panicking on a failed metadata fetch.

---

## File Structure

```
lofi-launcher-terminal/
  Cargo.toml                         # workspace manifest
  crates/
    lofi-common/
      Cargo.toml
      src/
        lib.rs
        config.rs                    # Config, Mood, load/save, path resolution
        protocol.rs                  # Command, Response, read/write framing
    lofi-daemon/
      Cargo.toml
      src/
        main.rs                      # wiring: load config, start mpv, serve socket
        mpv.rs                       # MpvController trait + real mpv IPC client
        state.rs                     # DaemonState: refcount + current mood/index
        server.rs                    # Unix socket accept loop, dispatch to state
    lofi-cli/
      Cargo.toml
      src/
        main.rs                      # clap CLI, subcommand dispatch
        client.rs                    # connect-or-spawn-daemon, send Command, read Response
        tui.rs                       # ratatui mood picker screen
  scripts/
    install.sh
    update.sh
    uninstall.sh
    lofi-launcher.sh.in              # shell snippet installed into rc files
  PKGBUILD
  config.default.toml                # shipped default config template
  README.md
```

`lofi-common` has no knowledge of sockets or mpv, only the config/protocol data shapes, so it is usable and testable in isolation from both binaries. `lofi-daemon` isolates mpv control behind a trait (`MpvController`) so refcount/dispatch logic is unit-testable without a real mpv process. `lofi-cli` isolates the daemon-connection logic (`client.rs`) from argument parsing (`main.rs`) and the TUI (`tui.rs`).

---

### Task 1: Workspace scaffold and config module

**Files:**
- Create: `Cargo.toml` (workspace root)
- Create: `crates/lofi-common/Cargo.toml`
- Create: `crates/lofi-common/src/lib.rs`
- Create: `crates/lofi-common/src/config.rs`
- Test: inline `#[cfg(test)]` module in `crates/lofi-common/src/config.rs`

**Interfaces:**
- Produces: `pub struct Config { pub default_mood: String, pub moods: std::collections::BTreeMap<String, Mood>, pub classifier: std::collections::BTreeMap<String, Vec<String>>, pub long_source_minutes: u32 }`, `pub struct Mood { pub sources: Vec<String> }`, `pub fn config_path() -> std::path::PathBuf`, `pub fn load_config(path: &std::path::Path) -> anyhow::Result<Config>`, `pub fn save_config(path: &std::path::Path, config: &Config) -> anyhow::Result<()>` (used starting in Task 9), `pub fn default_config_toml() -> &'static str`, `pub const BUILTIN_MOODS: [&str; 5] = ["code-and-chill", "deep-focus", "chill-beats", "rainy-day", "ambient"];`

- [ ] **Step 1: Create the workspace manifest**

```toml
[workspace]
resolver = "2"
members = ["crates/lofi-common", "crates/lofi-daemon", "crates/lofi-cli"]

[workspace.dependencies]
serde = { version = "1", features = ["derive"] }
serde_json = "1"
toml = "0.8"
anyhow = "1"
clap = { version = "4", features = ["derive"] }
```

- [ ] **Step 2: Create `crates/lofi-common/Cargo.toml`**

```toml
[package]
name = "lofi-common"
version = "0.1.0"
edition = "2021"

[dependencies]
serde = { workspace = true }
serde_json = { workspace = true }
toml = { workspace = true }
anyhow = { workspace = true }

[dev-dependencies]
tempfile = "3"
```

- [ ] **Step 3: Write the failing test for config parsing and path resolution**

In `crates/lofi-common/src/config.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    #[test]
    fn parses_default_config_template() {
        let cfg: Config = toml::from_str(default_config_toml()).unwrap();
        assert_eq!(cfg.default_mood, "code-and-chill");
        for name in BUILTIN_MOODS {
            assert!(cfg.moods.contains_key(name), "missing mood {name}");
        }
    }

    #[test]
    fn load_config_reads_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        let mut f = std::fs::File::create(&path).unwrap();
        write!(f, "{}", default_config_toml()).unwrap();
        let cfg = load_config(&path).unwrap();
        assert_eq!(cfg.default_mood, "code-and-chill");
    }

    #[test]
    fn config_path_respects_xdg_config_home() {
        std::env::set_var("XDG_CONFIG_HOME", "/tmp/lofi-test-xdg");
        let path = config_path();
        assert_eq!(path, std::path::PathBuf::from("/tmp/lofi-test-xdg/lofi-launcher/config.toml"));
        std::env::remove_var("XDG_CONFIG_HOME");
    }
}
```

- [ ] **Step 4: Run tests to verify they fail**

Run: `cargo test -p lofi-common`
Expected: FAIL to compile, no `Config`/`load_config`/`config_path`/`default_config_toml`/`BUILTIN_MOODS` defined yet.

- [ ] **Step 5: Implement the config module**

```rust
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::PathBuf;

pub const BUILTIN_MOODS: [&str; 5] = ["code-and-chill", "deep-focus", "chill-beats", "rainy-day", "ambient"];
pub const DEFAULT_LONG_SOURCE_MINUTES: u32 = 20;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Mood {
    #[serde(default)]
    pub sources: Vec<String>,
}

fn default_long_source_minutes() -> u32 {
    DEFAULT_LONG_SOURCE_MINUTES
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Config {
    pub default_mood: String,
    pub moods: BTreeMap<String, Mood>,
    #[serde(default)]
    pub classifier: BTreeMap<String, Vec<String>>,
    #[serde(default = "default_long_source_minutes")]
    pub long_source_minutes: u32,
}

pub fn default_config_toml() -> &'static str {
    include_str!("../../../config.default.toml")
}

pub fn config_path() -> PathBuf {
    let base = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            let home = std::env::var_os("HOME").expect("HOME must be set");
            PathBuf::from(home).join(".config")
        });
    base.join("lofi-launcher").join("config.toml")
}

pub fn load_config(path: &std::path::Path) -> anyhow::Result<Config> {
    let text = std::fs::read_to_string(path)?;
    let cfg: Config = toml::from_str(&text)?;
    Ok(cfg)
}
```

In `crates/lofi-common/src/lib.rs`:

```rust
pub mod config;
pub use config::{Config, Mood, BUILTIN_MOODS, config_path, load_config, default_config_toml};
```

- [ ] **Step 6: Create the shipped default config template**

`config.default.toml` at repo root:

```toml
default_mood = "code-and-chill"
long_source_minutes = 20

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

- [ ] **Step 7: Run tests to verify they pass**

Run: `cargo test -p lofi-common`
Expected: PASS, all three tests green.

- [ ] **Step 8: Commit**

```bash
git add Cargo.toml crates/lofi-common config.default.toml
git commit -m "Add workspace scaffold and config module"
```

---

### Task 2: IPC protocol module

**Files:**
- Modify: `crates/lofi-common/Cargo.toml` (no new deps needed, already has serde_json)
- Create: `crates/lofi-common/src/protocol.rs`
- Modify: `crates/lofi-common/src/lib.rs`
- Test: inline `#[cfg(test)]` module in `crates/lofi-common/src/protocol.rs`

**Interfaces:**
- Consumes: nothing beyond serde_json.
- Produces: `pub enum Command { Register, Unregister, Mood(String), Next, Pause, Resume, Status, Moods, Reload }`, `pub enum Response { Ok, Error(String), Status { mood: String, playing: bool, current_source: Option<String> }, Moods(Vec<String>) }`, `pub fn encode_command(cmd: &Command) -> String` (JSON + newline), `pub fn decode_command(line: &str) -> anyhow::Result<Command>`, `pub fn encode_response(resp: &Response) -> String`, `pub fn decode_response(line: &str) -> anyhow::Result<Response>`.

- [ ] **Step 1: Write the failing round-trip test**

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips_mood_command() {
        let cmd = Command::Mood("deep-focus".to_string());
        let line = encode_command(&cmd);
        assert!(line.ends_with('\n'));
        let decoded = decode_command(line.trim_end()).unwrap();
        match decoded {
            Command::Mood(name) => assert_eq!(name, "deep-focus"),
            other => panic!("unexpected {other:?}"),
        }
    }

    #[test]
    fn round_trips_status_response() {
        let resp = Response::Status {
            mood: "code-and-chill".to_string(),
            playing: true,
            current_source: Some("https://example.com/playlist".to_string()),
        };
        let line = encode_response(&resp);
        let decoded = decode_response(line.trim_end()).unwrap();
        match decoded {
            Response::Status { mood, playing, current_source } => {
                assert_eq!(mood, "code-and-chill");
                assert!(playing);
                assert_eq!(current_source.as_deref(), Some("https://example.com/playlist"));
            }
            other => panic!("unexpected {other:?}"),
        }
    }

    #[test]
    fn rejects_malformed_line() {
        assert!(decode_command("not json").is_err());
    }
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p lofi-common protocol`
Expected: FAIL to compile, `protocol` module does not exist.

- [ ] **Step 3: Implement the protocol module**

```rust
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "cmd", content = "arg")]
pub enum Command {
    Register,
    Unregister,
    Mood(String),
    Next,
    Pause,
    Resume,
    Status,
    Moods,
    Reload,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind")]
pub enum Response {
    Ok,
    Error(String),
    Status {
        mood: String,
        playing: bool,
        current_source: Option<String>,
    },
    Moods(Vec<String>),
}

pub fn encode_command(cmd: &Command) -> String {
    format!("{}\n", serde_json::to_string(cmd).expect("Command always serializes"))
}

pub fn decode_command(line: &str) -> anyhow::Result<Command> {
    Ok(serde_json::from_str(line)?)
}

pub fn encode_response(resp: &Response) -> String {
    format!("{}\n", serde_json::to_string(resp).expect("Response always serializes"))
}

pub fn decode_response(line: &str) -> anyhow::Result<Response> {
    Ok(serde_json::from_str(line)?)
}
```

Add `pub mod protocol;` and re-export in `crates/lofi-common/src/lib.rs`:

```rust
pub mod protocol;
pub use protocol::{Command, Response, encode_command, decode_command, encode_response, decode_response};
```

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test -p lofi-common`
Expected: PASS, all tests green (config tests from Task 1 plus these three).

- [ ] **Step 5: Commit**

```bash
git add crates/lofi-common/src/protocol.rs crates/lofi-common/src/lib.rs
git commit -m "Add IPC protocol types and framing"
```

---

### Task 3: Daemon state machine (refcount and mood selection, no real mpv)

**Files:**
- Create: `crates/lofi-daemon/Cargo.toml`
- Create: `crates/lofi-daemon/src/mpv.rs`
- Create: `crates/lofi-daemon/src/state.rs`
- Test: inline `#[cfg(test)]` module in `crates/lofi-daemon/src/state.rs`

**Interfaces:**
- Consumes: `lofi_common::{Config, Mood, Command, Response}`.
- Produces: `pub trait MpvController { fn start_source(&mut self, source: &str) -> anyhow::Result<()>; fn stop(&mut self) -> anyhow::Result<()>; fn pause(&mut self) -> anyhow::Result<()>; fn resume(&mut self) -> anyhow::Result<()>; fn last_error(&self) -> Option<String>; }`, `pub struct DaemonState<M: MpvController> { ... }` with `pub fn new(config: Config, mpv: M) -> Self`, `pub fn handle(&mut self, cmd: Command) -> Response`.

- [ ] **Step 1: Write the failing tests for refcount and mood dispatch**

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use lofi_common::{Config, Mood};
    use std::collections::BTreeMap;

    #[derive(Default)]
    struct FakeMpv {
        started: Vec<String>,
        stopped: bool,
        paused: bool,
    }

    impl MpvController for FakeMpv {
        fn start_source(&mut self, source: &str) -> anyhow::Result<()> {
            self.started.push(source.to_string());
            self.stopped = false;
            Ok(())
        }
        fn stop(&mut self) -> anyhow::Result<()> {
            self.stopped = true;
            Ok(())
        }
        fn pause(&mut self) -> anyhow::Result<()> {
            self.paused = true;
            Ok(())
        }
        fn resume(&mut self) -> anyhow::Result<()> {
            self.paused = false;
            Ok(())
        }
        fn last_error(&self) -> Option<String> {
            None
        }
    }

    fn test_config() -> Config {
        let mut moods = BTreeMap::new();
        moods.insert("code-and-chill".to_string(), Mood { sources: vec!["a.mp3".to_string()] });
        moods.insert("deep-focus".to_string(), Mood { sources: vec![] });
        Config {
            default_mood: "code-and-chill".to_string(),
            moods,
            classifier: BTreeMap::new(),
            long_source_minutes: 20,
        }
    }

    #[test]
    fn register_starts_playback_only_on_first_session() {
        let mut state = DaemonState::new(test_config(), FakeMpv::default());
        assert!(matches!(state.handle(Command::Register), Response::Ok));
        assert_eq!(state.mpv.started, vec!["a.mp3".to_string()]);
        state.mpv.started.clear();
        assert!(matches!(state.handle(Command::Register), Response::Ok));
        assert!(state.mpv.started.is_empty(), "second register must not restart playback");
    }

    #[test]
    fn unregister_stops_playback_only_when_count_reaches_zero() {
        let mut state = DaemonState::new(test_config(), FakeMpv::default());
        state.handle(Command::Register);
        state.handle(Command::Register);
        state.handle(Command::Unregister);
        assert!(!state.mpv.stopped, "must not stop while one session remains");
        state.handle(Command::Unregister);
        assert!(state.mpv.stopped, "must stop when last session closes");
    }

    #[test]
    fn unregister_below_zero_is_a_no_op() {
        let mut state = DaemonState::new(test_config(), FakeMpv::default());
        assert!(matches!(state.handle(Command::Unregister), Response::Ok));
        assert_eq!(state.session_count(), 0);
    }

    #[test]
    fn mood_switch_to_unknown_mood_returns_error_listing_valid_names() {
        let mut state = DaemonState::new(test_config(), FakeMpv::default());
        let resp = state.handle(Command::Mood("not-a-mood".to_string()));
        match resp {
            Response::Error(msg) => {
                assert!(msg.contains("code-and-chill"));
                assert!(msg.contains("deep-focus"));
            }
            other => panic!("expected error, got {other:?}"),
        }
    }

    #[test]
    fn mood_with_empty_sources_reports_warning_instead_of_starting() {
        let mut state = DaemonState::new(test_config(), FakeMpv::default());
        state.handle(Command::Register);
        let resp = state.handle(Command::Mood("deep-focus".to_string()));
        match resp {
            Response::Error(msg) => assert!(msg.contains("no sources configured")),
            other => panic!("expected warning error, got {other:?}"),
        }
    }
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test -p lofi-daemon state`
Expected: FAIL to compile, crate and module do not exist yet.

- [ ] **Step 3: Create `crates/lofi-daemon/Cargo.toml`**

```toml
[package]
name = "lofi-daemon"
version = "0.1.0"
edition = "2021"

[dependencies]
lofi-common = { path = "../lofi-common" }
serde_json = { workspace = true }
anyhow = { workspace = true }
```

- [ ] **Step 4: Implement `mpv.rs` with the controller trait (real impl comes in Task 4)**

```rust
pub trait MpvController {
    fn start_source(&mut self, source: &str) -> anyhow::Result<()>;
    fn stop(&mut self) -> anyhow::Result<()>;
    fn pause(&mut self) -> anyhow::Result<()>;
    fn resume(&mut self) -> anyhow::Result<()>;
    fn last_error(&self) -> Option<String>;
}
```

- [ ] **Step 5: Implement `state.rs`**

```rust
use crate::mpv::MpvController;
use lofi_common::{Command, Config, Response};

pub struct DaemonState<M: MpvController> {
    config: Config,
    pub mpv: M,
    session_count: u32,
    current_mood: String,
    current_index: usize,
    playing: bool,
}

impl<M: MpvController> DaemonState<M> {
    pub fn new(config: Config, mpv: M) -> Self {
        let current_mood = config.default_mood.clone();
        Self {
            config,
            mpv,
            session_count: 0,
            current_mood,
            current_index: 0,
            playing: false,
        }
    }

    pub fn session_count(&self) -> u32 {
        self.session_count
    }

    fn start_current_mood(&mut self) -> Response {
        let mood = match self.config.moods.get(&self.current_mood) {
            Some(m) => m,
            None => return Response::Error(format!("unknown mood: {}", self.current_mood)),
        };
        match mood.sources.get(self.current_index) {
            Some(source) => match self.mpv.start_source(source) {
                Ok(()) => {
                    self.playing = true;
                    Response::Ok
                }
                Err(e) => Response::Error(e.to_string()),
            },
            None => Response::Error(format!("mood '{}' has no sources configured", self.current_mood)),
        }
    }

    fn valid_mood_names(&self) -> String {
        self.config.moods.keys().cloned().collect::<Vec<_>>().join(", ")
    }

    pub fn handle(&mut self, cmd: Command) -> Response {
        match cmd {
            Command::Register => {
                self.session_count += 1;
                if self.session_count == 1 {
                    self.start_current_mood()
                } else {
                    Response::Ok
                }
            }
            Command::Unregister => {
                if self.session_count > 0 {
                    self.session_count -= 1;
                }
                if self.session_count == 0 && self.playing {
                    self.playing = false;
                    match self.mpv.stop() {
                        Ok(()) => Response::Ok,
                        Err(e) => Response::Error(e.to_string()),
                    }
                } else {
                    Response::Ok
                }
            }
            Command::Mood(name) => {
                if !self.config.moods.contains_key(&name) {
                    return Response::Error(format!(
                        "unknown mood '{name}', valid moods: {}",
                        self.valid_mood_names()
                    ));
                }
                self.current_mood = name;
                self.current_index = 0;
                if self.session_count > 0 {
                    self.start_current_mood()
                } else {
                    Response::Ok
                }
            }
            Command::Next => {
                let len = self.config.moods.get(&self.current_mood).map(|m| m.sources.len()).unwrap_or(0);
                if len == 0 {
                    return Response::Error(format!("mood '{}' has no sources configured", self.current_mood));
                }
                self.current_index = (self.current_index + 1) % len;
                self.start_current_mood()
            }
            Command::Pause => match self.mpv.pause() {
                Ok(()) => Response::Ok,
                Err(e) => Response::Error(e.to_string()),
            },
            Command::Resume => match self.mpv.resume() {
                Ok(()) => Response::Ok,
                Err(e) => Response::Error(e.to_string()),
            },
            Command::Status => {
                let current_source = self
                    .config
                    .moods
                    .get(&self.current_mood)
                    .and_then(|m| m.sources.get(self.current_index))
                    .cloned();
                Response::Status {
                    mood: self.current_mood.clone(),
                    playing: self.playing,
                    current_source,
                }
            }
            Command::Moods => Response::Moods(self.config.moods.keys().cloned().collect()),
            Command::Reload => Response::Ok,
        }
    }
}
```

- [ ] **Step 6: Run tests to verify they pass**

Run: `cargo test -p lofi-daemon`
Expected: PASS, all five tests green.

- [ ] **Step 7: Commit**

```bash
git add crates/lofi-daemon/Cargo.toml crates/lofi-daemon/src/mpv.rs crates/lofi-daemon/src/state.rs
git commit -m "Add daemon refcount state machine with fake mpv controller"
```

---

### Task 4: Real mpv controller over its JSON IPC socket

**Files:**
- Modify: `crates/lofi-daemon/Cargo.toml` (no new deps, uses std only)
- Modify: `crates/lofi-daemon/src/mpv.rs`
- Test: inline `#[cfg(test)]` module in `crates/lofi-daemon/src/mpv.rs`, gated behind an mpv-present check so CI without mpv still passes

**Interfaces:**
- Consumes: `MpvController` trait from Task 3.
- Produces: `pub struct RealMpv { socket_path: std::path::PathBuf, child: std::process::Child }` with `pub fn spawn(socket_path: std::path::PathBuf) -> anyhow::Result<Self>` and the `MpvController` impl. Also produces `fn find_mpris_script() -> Option<String>`, used internally by `spawn` to add an `mpv-mpris` `--script=` flag when present, with no further consumers outside this file.

- [ ] **Step 1: Write a test that is skipped gracefully when mpv is not installed, plus a test for MPRIS script discovery**

```rust
#[cfg(test)]
mod tests {
    use super::*;

    fn mpv_available() -> bool {
        std::process::Command::new("mpv").arg("--version").output().is_ok()
    }

    #[test]
    fn spawns_and_accepts_ipc_commands() {
        if !mpv_available() {
            eprintln!("skipping: mpv not installed in this environment");
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        let socket_path = dir.path().join("mpv-test.sock");
        let mut mpv = RealMpv::spawn(socket_path).unwrap();
        // pausing an idle mpv instance must not error, proving the IPC link works
        mpv.pause().unwrap();
        mpv.resume().unwrap();
        mpv.stop().unwrap();
    }

    #[test]
    fn find_mpris_script_prefers_env_override_when_file_exists() {
        let dir = tempfile::tempdir().unwrap();
        let fake_script = dir.path().join("mpris.so");
        std::fs::write(&fake_script, b"").unwrap();
        std::env::set_var("LOFI_MPV_MPRIS_SCRIPT", fake_script.to_str().unwrap());
        assert_eq!(find_mpris_script(), Some(fake_script.to_str().unwrap().to_string()));
        std::env::remove_var("LOFI_MPV_MPRIS_SCRIPT");
    }

    #[test]
    fn find_mpris_script_ignores_env_override_pointing_at_missing_file() {
        std::env::set_var("LOFI_MPV_MPRIS_SCRIPT", "/nonexistent/mpris.so");
        let result = find_mpris_script();
        std::env::remove_var("LOFI_MPV_MPRIS_SCRIPT");
        assert_ne!(result, Some("/nonexistent/mpris.so".to_string()));
    }
}
```

- [ ] **Step 2: Add `tempfile` as a dev-dependency**

In `crates/lofi-daemon/Cargo.toml`, add under `[dev-dependencies]`:

```toml
[dev-dependencies]
tempfile = "3"
```

- [ ] **Step 3: Run test to verify current behavior (compiles, fails to find `RealMpv::spawn`)**

Run: `cargo test -p lofi-daemon mpv`
Expected: FAIL to compile, `RealMpv` not defined.

- [ ] **Step 4: Implement `RealMpv`**

```rust
use std::io::{Read, Write};
use std::os::unix::net::UnixStream;
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

pub struct RealMpv {
    socket_path: PathBuf,
    child: Child,
}

fn mpv_binary() -> String {
    std::env::var("LOFI_MPV_BIN").unwrap_or_else(|_| "mpv".to_string())
}

const MPRIS_SCRIPT_CANDIDATES: [&str; 3] = [
    "/usr/share/mpv/scripts/mpris.so",
    "/usr/lib/mpv/scripts/mpris.so",
    "/usr/local/share/mpv/scripts/mpris.so",
];

fn find_mpris_script() -> Option<String> {
    if let Ok(path) = std::env::var("LOFI_MPV_MPRIS_SCRIPT") {
        if std::path::Path::new(&path).exists() {
            return Some(path);
        }
        eprintln!("LOFI_MPV_MPRIS_SCRIPT is set to '{path}' but that file does not exist, ignoring");
    }
    MPRIS_SCRIPT_CANDIDATES
        .iter()
        .find(|p| std::path::Path::new(p).exists())
        .map(|p| p.to_string())
}

impl RealMpv {
    pub fn spawn(socket_path: PathBuf) -> anyhow::Result<Self> {
        let ipc_arg = format!("--input-ipc-server={}", socket_path.display());
        let mut command = Command::new(mpv_binary());
        command.arg("--idle").arg("--no-video").arg(ipc_arg);
        match find_mpris_script() {
            Some(script) => {
                command.arg(format!("--script={script}"));
            }
            None => {
                eprintln!("mpv-mpris not detected, now-playing widgets won't see this player");
            }
        }
        let child = command
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()?;

        let deadline = Instant::now() + Duration::from_secs(3);
        while !socket_path.exists() {
            if Instant::now() > deadline {
                anyhow::bail!("mpv did not create its IPC socket within 3 seconds");
            }
            std::thread::sleep(Duration::from_millis(50));
        }

        Ok(Self { socket_path, child })
    }

    fn send(&self, payload: serde_json::Value) -> anyhow::Result<serde_json::Value> {
        let mut stream = UnixStream::connect(&self.socket_path)?;
        let mut line = serde_json::to_string(&payload)?;
        line.push('\n');
        stream.write_all(line.as_bytes())?;
        stream.set_read_timeout(Some(Duration::from_secs(2)))?;
        let mut buf = String::new();
        stream.read_to_string(&mut buf)?;
        let first_line = buf.lines().next().unwrap_or("{}");
        Ok(serde_json::from_str(first_line)?)
    }
}

impl super::mpv::MpvController for RealMpv {
    fn start_source(&mut self, source: &str) -> anyhow::Result<()> {
        self.send(serde_json::json!({ "command": ["loadfile", source, "replace"] }))?;
        Ok(())
    }

    fn stop(&mut self) -> anyhow::Result<()> {
        self.send(serde_json::json!({ "command": ["stop"] }))?;
        Ok(())
    }

    fn pause(&mut self) -> anyhow::Result<()> {
        self.send(serde_json::json!({ "command": ["set_property", "pause", true] }))?;
        Ok(())
    }

    fn resume(&mut self) -> anyhow::Result<()> {
        self.send(serde_json::json!({ "command": ["set_property", "pause", false] }))?;
        Ok(())
    }

    fn last_error(&self) -> Option<String> {
        None
    }
}

impl Drop for RealMpv {
    fn drop(&mut self) {
        let _ = self.child.kill();
    }
}
```

Note: `RealMpv` lives in `mpv.rs` alongside the trait; the `super::mpv::MpvController` reference in the snippet above assumes the impl block stays in the same file as the trait, so write it as `impl MpvController for RealMpv` directly since both are in `mpv.rs`.

- [ ] **Step 5: Run test to verify it passes (or skips cleanly)**

Run: `cargo test -p lofi-daemon mpv`
Expected: PASS if mpv is installed, or PASS with a printed skip message if not. Either way, no compile errors and no panics.

- [ ] **Step 6: Commit**

```bash
git add crates/lofi-daemon/Cargo.toml crates/lofi-daemon/src/mpv.rs
git commit -m "Add real mpv controller over its JSON IPC socket"
```

---

### Task 5: Daemon Unix socket server and main wiring

**Files:**
- Create: `crates/lofi-daemon/src/server.rs`
- Create: `crates/lofi-daemon/src/main.rs`
- Modify: `crates/lofi-daemon/src/mpv.rs` (add `pub mod` wiring is not needed here, just confirm it compiles as a module, see Step 1)
- Test: inline `#[cfg(test)]` module in `crates/lofi-daemon/src/server.rs`

**Interfaces:**
- Consumes: `DaemonState<M>` and `MpvController` from Tasks 3 to 4, `lofi_common::{Command, Response, encode_command, decode_command, encode_response, decode_response}`.
- Produces: `pub fn serve<M: MpvController>(socket_path: &std::path::Path, state: std::sync::Arc<std::sync::Mutex<DaemonState<M>>>) -> anyhow::Result<()>` which blocks accepting connections until the process exits.

- [ ] **Step 1: Write a failing test that runs the server against a fake mpv and talks to it over a real Unix socket**

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::mpv::MpvController;
    use crate::state::DaemonState;
    use lofi_common::{Config, Mood};
    use std::collections::BTreeMap;
    use std::io::{BufRead, BufReader, Write};
    use std::os::unix::net::UnixStream;
    use std::sync::{Arc, Mutex};

    #[derive(Default)]
    struct NoopMpv;
    impl MpvController for NoopMpv {
        fn start_source(&mut self, _source: &str) -> anyhow::Result<()> { Ok(()) }
        fn stop(&mut self) -> anyhow::Result<()> { Ok(()) }
        fn pause(&mut self) -> anyhow::Result<()> { Ok(()) }
        fn resume(&mut self) -> anyhow::Result<()> { Ok(()) }
        fn last_error(&self) -> Option<String> { None }
    }

    #[test]
    fn server_responds_to_status_over_socket() {
        let dir = tempfile::tempdir().unwrap();
        let socket_path = dir.path().join("daemon-test.sock");

        let mut moods = BTreeMap::new();
        moods.insert("code-and-chill".to_string(), Mood { sources: vec!["a.mp3".to_string()] });
        let config = Config {
            default_mood: "code-and-chill".to_string(),
            moods,
            classifier: BTreeMap::new(),
            long_source_minutes: 20,
        };
        let state = Arc::new(Mutex::new(DaemonState::new(config, NoopMpv)));

        let server_socket_path = socket_path.clone();
        let server_state = state.clone();
        std::thread::spawn(move || {
            serve(&server_socket_path, server_state).unwrap();
        });

        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
        while !socket_path.exists() {
            if std::time::Instant::now() > deadline {
                panic!("server never created its socket");
            }
            std::thread::sleep(std::time::Duration::from_millis(20));
        }

        let mut stream = UnixStream::connect(&socket_path).unwrap();
        let line = lofi_common::encode_command(&lofi_common::Command::Status);
        stream.write_all(line.as_bytes()).unwrap();
        let mut reader = BufReader::new(stream);
        let mut response_line = String::new();
        reader.read_line(&mut response_line).unwrap();
        let resp = lofi_common::decode_response(response_line.trim_end()).unwrap();
        match resp {
            lofi_common::Response::Status { mood, .. } => assert_eq!(mood, "code-and-chill"),
            other => panic!("unexpected {other:?}"),
        }
    }
}
```

- [ ] **Step 2: Add `tempfile` dev-dependency if not already present from Task 4**

Already added in Task 4's `Cargo.toml` edit; no further change needed.

- [ ] **Step 3: Run test to verify it fails**

Run: `cargo test -p lofi-daemon server`
Expected: FAIL to compile, `serve` not defined.

- [ ] **Step 4: Implement `server.rs`**

```rust
use crate::mpv::MpvController;
use crate::state::DaemonState;
use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::{UnixListener, UnixStream};
use std::sync::{Arc, Mutex};

pub fn serve<M: MpvController>(
    socket_path: &std::path::Path,
    state: Arc<Mutex<DaemonState<M>>>,
) -> anyhow::Result<()> {
    if socket_path.exists() {
        std::fs::remove_file(socket_path)?;
    }
    let listener = UnixListener::bind(socket_path)?;
    for incoming in listener.incoming() {
        let stream = incoming?;
        handle_connection(stream, &state);
    }
    Ok(())
}

fn handle_connection<M: MpvController>(stream: UnixStream, state: &Arc<Mutex<DaemonState<M>>>) {
    let mut writer = match stream.try_clone() {
        Ok(w) => w,
        Err(_) => return,
    };
    let reader = BufReader::new(stream);
    for line in reader.lines() {
        let line = match line {
            Ok(l) => l,
            Err(_) => return,
        };
        if line.is_empty() {
            continue;
        }
        let response = match lofi_common::decode_command(&line) {
            Ok(cmd) => {
                let mut guard = state.lock().expect("daemon state mutex poisoned");
                guard.handle(cmd)
            }
            Err(e) => lofi_common::Response::Error(format!("malformed command: {e}")),
        };
        let encoded = lofi_common::encode_response(&response);
        if writer.write_all(encoded.as_bytes()).is_err() {
            return;
        }
    }
}
```

Add `pub mod server;` and `pub mod state;` and `pub mod mpv;` near the top of `main.rs`.

- [ ] **Step 5: Implement `main.rs`**

```rust
mod mpv;
mod server;
mod state;

use mpv::RealMpv;
use state::DaemonState;
use std::sync::{Arc, Mutex};

fn runtime_dir() -> std::path::PathBuf {
    std::env::var_os("XDG_RUNTIME_DIR")
        .map(std::path::PathBuf::from)
        .expect("XDG_RUNTIME_DIR must be set; lofi-daemon targets Linux session environments")
}

fn main() -> anyhow::Result<()> {
    let config_path = lofi_common::config_path();
    let config = if config_path.exists() {
        lofi_common::load_config(&config_path)?
    } else {
        toml::from_str(lofi_common::default_config_toml())?
    };

    let mpv_binary_check = std::process::Command::new(std::env::var("LOFI_MPV_BIN").unwrap_or_else(|_| "mpv".to_string()))
        .arg("--version")
        .output();
    if mpv_binary_check.is_err() {
        eprintln!("mpv not detected, install it via your distro's package manager");
        std::process::exit(1);
    }

    let run_dir = runtime_dir();
    let mpv_socket = run_dir.join("lofi-mpv.sock");
    let daemon_socket = run_dir.join("lofi-daemon.sock");

    let mpv = RealMpv::spawn(mpv_socket)?;
    let state = Arc::new(Mutex::new(DaemonState::new(config, mpv)));

    server::serve(&daemon_socket, state)
}
```

- [ ] **Step 6: Run tests to verify they pass**

Run: `cargo test -p lofi-daemon`
Expected: PASS for all tests across `mpv.rs`, `state.rs`, and `server.rs`.

- [ ] **Step 7: Run a full workspace build to confirm the daemon binary compiles**

Run: `cargo build`
Expected: builds `lofi-common` and `lofi-daemon` (lofi-cli crate does not exist yet, so build this with `cargo build -p lofi-common -p lofi-daemon` if the workspace members line in Task 1 already lists `lofi-cli` as a member before it exists).

Since Task 1's workspace `members` list already names `crates/lofi-cli`, which does not exist until Task 6, update the workspace `Cargo.toml` now to comment it out temporarily, or add a minimal placeholder. Add a minimal placeholder to avoid a broken build between tasks:

```bash
mkdir -p crates/lofi-cli/src
cat > crates/lofi-cli/Cargo.toml <<'EOF'
[package]
name = "lofi-cli"
version = "0.1.0"
edition = "2021"

[[bin]]
name = "lofi"
path = "src/main.rs"
EOF
cat > crates/lofi-cli/src/main.rs <<'EOF'
fn main() {
    println!("lofi CLI placeholder, implemented in Task 6");
}
EOF
```

Run: `cargo build`
Expected: builds cleanly across all three workspace members.

- [ ] **Step 8: Commit**

```bash
git add crates/lofi-daemon/src/server.rs crates/lofi-daemon/src/main.rs crates/lofi-cli
git commit -m "Add daemon Unix socket server and main wiring"
```

---

### Task 6: CLI client and subcommands

**Files:**
- Modify: `crates/lofi-cli/Cargo.toml`
- Create: `crates/lofi-cli/src/client.rs`
- Modify: `crates/lofi-cli/src/main.rs`
- Test: inline `#[cfg(test)]` module in `crates/lofi-cli/src/client.rs`

**Interfaces:**
- Consumes: `lofi_common::{Command, Response, encode_command, decode_response}`.
- Produces: `pub fn send_command(socket_path: &std::path::Path, cmd: &lofi_common::Command) -> anyhow::Result<lofi_common::Response>`, `pub fn ensure_daemon_running(socket_path: &std::path::Path) -> anyhow::Result<()>` (spawns `lofi-daemon` detached if the socket is missing or stale).

- [ ] **Step 1: Write a failing test for `send_command` against a minimal fake server**

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{BufRead, BufReader, Write};
    use std::os::unix::net::UnixListener;

    #[test]
    fn send_command_round_trips_against_fake_server() {
        let dir = tempfile::tempdir().unwrap();
        let socket_path = dir.path().join("fake-daemon.sock");
        let listener = UnixListener::bind(&socket_path).unwrap();

        let server_socket_path = socket_path.clone();
        std::thread::spawn(move || {
            let (stream, _) = listener.accept().unwrap();
            let mut writer = stream.try_clone().unwrap();
            let reader = BufReader::new(stream);
            for line in reader.lines() {
                let _ = line.unwrap();
                let resp = lofi_common::encode_response(&lofi_common::Response::Ok);
                writer.write_all(resp.as_bytes()).unwrap();
                break;
            }
            let _ = server_socket_path;
        });

        std::thread::sleep(std::time::Duration::from_millis(50));
        let resp = send_command(&socket_path, &lofi_common::Command::Register).unwrap();
        assert!(matches!(resp, lofi_common::Response::Ok));
    }
}
```

- [ ] **Step 2: Update `crates/lofi-cli/Cargo.toml`**

```toml
[package]
name = "lofi-cli"
version = "0.1.0"
edition = "2021"

[[bin]]
name = "lofi"
path = "src/main.rs"

[dependencies]
lofi-common = { path = "../lofi-common" }
clap = { workspace = true }
anyhow = { workspace = true }

[dev-dependencies]
tempfile = "3"
```

- [ ] **Step 3: Run test to verify it fails**

Run: `cargo test -p lofi-cli`
Expected: FAIL to compile, `client` module and `send_command` not defined.

- [ ] **Step 4: Implement `client.rs`**

```rust
use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::path::Path;
use std::time::Duration;

pub fn send_command(socket_path: &Path, cmd: &lofi_common::Command) -> anyhow::Result<lofi_common::Response> {
    let mut stream = UnixStream::connect(socket_path)?;
    stream.set_read_timeout(Some(Duration::from_secs(5)))?;
    let line = lofi_common::encode_command(cmd);
    stream.write_all(line.as_bytes())?;
    let mut reader = BufReader::new(stream);
    let mut response_line = String::new();
    reader.read_line(&mut response_line)?;
    if response_line.is_empty() {
        anyhow::bail!("daemon closed the connection without a response");
    }
    lofi_common::decode_response(response_line.trim_end())
}

pub fn ensure_daemon_running(socket_path: &Path) -> anyhow::Result<()> {
    if socket_path.exists() && UnixStream::connect(socket_path).is_ok() {
        return Ok(());
    }
    if socket_path.exists() {
        std::fs::remove_file(socket_path)?;
    }
    std::process::Command::new("lofi-daemon")
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()?;

    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    while !socket_path.exists() {
        if std::time::Instant::now() > deadline {
            anyhow::bail!("lofi-daemon did not start within 5 seconds");
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    Ok(())
}
```

- [ ] **Step 5: Run test to verify it passes**

Run: `cargo test -p lofi-cli`
Expected: PASS.

- [ ] **Step 6: Implement the CLI subcommands in `main.rs`**

```rust
mod client;

use clap::{Parser, Subcommand};
use lofi_common::Command as DaemonCommand;

#[derive(Parser)]
#[command(name = "lofi")]
struct Cli {
    #[command(subcommand)]
    command: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    Register,
    Unregister,
    Mood { name: String },
    Next,
    Pause,
    Resume,
    Status,
    Moods,
    Tui,
}

fn runtime_socket() -> std::path::PathBuf {
    let run_dir = std::env::var_os("XDG_RUNTIME_DIR")
        .map(std::path::PathBuf::from)
        .expect("XDG_RUNTIME_DIR must be set; lofi targets Linux session environments");
    run_dir.join("lofi-daemon.sock")
}

fn print_response(resp: lofi_common::Response) {
    match resp {
        lofi_common::Response::Ok => println!("ok"),
        lofi_common::Response::Error(msg) => eprintln!("error: {msg}"),
        lofi_common::Response::Status { mood, playing, current_source } => {
            let state = if playing { "playing" } else { "paused" };
            let source = current_source.unwrap_or_else(|| "none".to_string());
            println!("mood: {mood}\nstate: {state}\nsource: {source}");
        }
        lofi_common::Response::Moods(names) => {
            for name in names {
                println!("{name}");
            }
        }
    }
}

fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();
    let socket = runtime_socket();

    if matches!(cli.command, Cmd::Tui) {
        return tui::run(socket);
    }

    client::ensure_daemon_running(&socket)?;

    let cmd = match cli.command {
        Cmd::Register => DaemonCommand::Register,
        Cmd::Unregister => DaemonCommand::Unregister,
        Cmd::Mood { name } => DaemonCommand::Mood(name),
        Cmd::Next => DaemonCommand::Next,
        Cmd::Pause => DaemonCommand::Pause,
        Cmd::Resume => DaemonCommand::Resume,
        Cmd::Status => DaemonCommand::Status,
        Cmd::Moods => DaemonCommand::Moods,
        Cmd::Tui => unreachable!("handled above"),
    };

    let resp = client::send_command(&socket, &cmd)?;
    print_response(resp);
    Ok(())
}

mod tui;
```

- [ ] **Step 7: Add a placeholder `tui.rs` so the crate compiles (full TUI built in Task 7)**

```rust
pub fn run(_socket: std::path::PathBuf) -> anyhow::Result<()> {
    println!("lofi tui placeholder, implemented in Task 7");
    Ok(())
}
```

- [ ] **Step 8: Run tests and build to verify everything passes**

Run: `cargo test -p lofi-cli && cargo build`
Expected: PASS and clean build across the workspace.

- [ ] **Step 9: Commit**

```bash
git add crates/lofi-cli
git commit -m "Add CLI client and subcommands"
```

---

### Task 7: TUI mood picker

**Files:**
- Modify: `crates/lofi-cli/Cargo.toml`
- Modify: `crates/lofi-cli/src/tui.rs`

**Interfaces:**
- Consumes: `client::send_command`, `lofi_common::{Command, Response}`.
- Produces: `pub fn run(socket: std::path::PathBuf) -> anyhow::Result<()>` (replaces the Task 6 placeholder; no further consumers).

- [ ] **Step 1: Add `ratatui` and `crossterm` dependencies**

In `crates/lofi-cli/Cargo.toml`, under `[dependencies]`:

```toml
ratatui = "0.28"
crossterm = "0.28"
```

- [ ] **Step 2: Implement the TUI**

This task has no automated test since it is an interactive terminal UI; it is verified manually in Task 8's end-to-end check. Replace `crates/lofi-cli/src/tui.rs`:

```rust
use crate::client;
use crossterm::event::{self, Event, KeyCode};
use crossterm::terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen};
use crossterm::ExecutableCommand;
use ratatui::prelude::*;
use ratatui::widgets::{Block, Borders, List, ListItem, ListState};
use std::io::stdout;
use std::path::PathBuf;

pub fn run(socket: PathBuf) -> anyhow::Result<()> {
    let moods = match client::send_command(&socket, &lofi_common::Command::Moods)? {
        lofi_common::Response::Moods(names) => names,
        other => anyhow::bail!("unexpected response listing moods: {other:?}"),
    };

    enable_raw_mode()?;
    stdout().execute(EnterAlternateScreen)?;
    let backend = CrosstermBackend::new(stdout());
    let mut terminal = Terminal::new(backend)?;

    let mut selected = 0usize;
    let result = loop {
        terminal.draw(|frame| {
            let items: Vec<ListItem> = moods.iter().map(|m| ListItem::new(m.as_str())).collect();
            let mut state = ListState::default();
            state.select(Some(selected));
            let list = List::new(items)
                .block(Block::default().borders(Borders::ALL).title("lofi mood (enter: select, p: pause, r: resume, n: next, q: quit)"))
                .highlight_symbol(">> ");
            frame.render_stateful_widget(list, frame.area(), &mut state);
        })?;

        if event::poll(std::time::Duration::from_millis(200))? {
            if let Event::Key(key) = event::read()? {
                match key.code {
                    KeyCode::Up => {
                        if selected > 0 {
                            selected -= 1;
                        }
                    }
                    KeyCode::Down => {
                        if selected + 1 < moods.len() {
                            selected += 1;
                        }
                    }
                    KeyCode::Enter => {
                        let name = moods[selected].clone();
                        let _ = client::send_command(&socket, &lofi_common::Command::Mood(name));
                    }
                    KeyCode::Char('p') => {
                        let _ = client::send_command(&socket, &lofi_common::Command::Pause);
                    }
                    KeyCode::Char('r') => {
                        let _ = client::send_command(&socket, &lofi_common::Command::Resume);
                    }
                    KeyCode::Char('n') => {
                        let _ = client::send_command(&socket, &lofi_common::Command::Next);
                    }
                    KeyCode::Char('q') => break Ok(()),
                    _ => {}
                }
            }
        }
    };

    disable_raw_mode()?;
    stdout().execute(LeaveAlternateScreen)?;
    result
}
```

- [ ] **Step 3: Build to verify it compiles**

Run: `cargo build`
Expected: clean build across the workspace.

- [ ] **Step 4: Manual check**

Run: `cargo run -p lofi-cli --bin lofi -- tui` with a daemon already running (start one with `cargo run -p lofi-daemon &` first) and confirm arrow keys move the selection, Enter switches mood, `p`/`r`/`n` send pause/resume/next, and `q` exits cleanly, restoring the normal terminal screen.

- [ ] **Step 5: Commit**

```bash
git add crates/lofi-cli/Cargo.toml crates/lofi-cli/src/tui.rs
git commit -m "Add TUI mood picker"
```

---

### Task 8: Install, update, and uninstall scripts with shell integration

**Files:**
- Create: `scripts/install.sh`
- Create: `scripts/update.sh`
- Create: `scripts/uninstall.sh`
- Create: `scripts/lofi-launcher.sh.in`

**Interfaces:**
- Consumes: the built `lofi` and `lofi-daemon` binaries from `target/release/`.
- Produces: nothing consumed by later Rust code; this is the user-facing lifecycle surface.

- [ ] **Step 1: Create the shell integration snippet template**

`scripts/lofi-launcher.sh.in`:

```sh
# lofi-launcher-terminal: start on shell open, stop on shell close
lofi register >/dev/null 2>&1
trap 'lofi unregister >/dev/null 2>&1' EXIT
```

- [ ] **Step 2: Write `scripts/install.sh`**

```sh
#!/usr/bin/env sh
set -eu

REPO_DIR="$(cd "$(dirname "$0")/.." && pwd)"
PREFIX="${PREFIX:-$HOME/.local}"
BIN_DIR="$PREFIX/bin"
CONFIG_DIR="${XDG_CONFIG_HOME:-$HOME/.config}/lofi-launcher"
MARKER_START="# >>> lofi-launcher-terminal >>>"
MARKER_END="# <<< lofi-launcher-terminal <<<"

echo "Building release binaries..."
(cd "$REPO_DIR" && cargo build --release)

mkdir -p "$BIN_DIR"
cp "$REPO_DIR/target/release/lofi" "$BIN_DIR/lofi"
cp "$REPO_DIR/target/release/lofi-daemon" "$BIN_DIR/lofi-daemon"
echo "Installed binaries to $BIN_DIR"

mkdir -p "$CONFIG_DIR"
if [ ! -f "$CONFIG_DIR/config.toml" ]; then
    cp "$REPO_DIR/config.default.toml" "$CONFIG_DIR/config.toml"
    echo "Created default config at $CONFIG_DIR/config.toml"
else
    echo "Existing config at $CONFIG_DIR/config.toml left untouched"
fi

case "${SHELL:-}" in
    */zsh) RC_FILE="$HOME/.zshrc" ;;
    */bash) RC_FILE="$HOME/.bashrc" ;;
    *)
        echo "Could not detect bash or zsh from \$SHELL ($SHELL); add scripts/lofi-launcher.sh.in to your shell rc file manually."
        RC_FILE=""
        ;;
esac

if [ -n "$RC_FILE" ]; then
    if [ -f "$RC_FILE" ] && grep -qF "$MARKER_START" "$RC_FILE"; then
        echo "Shell integration already present in $RC_FILE"
    else
        {
            echo "$MARKER_START"
            cat "$REPO_DIR/scripts/lofi-launcher.sh.in"
            echo "$MARKER_END"
        } >> "$RC_FILE"
        echo "Added shell integration to $RC_FILE"
    fi
fi

if ! command -v mpv >/dev/null 2>&1; then
    echo "Warning: mpv not detected on PATH, install it via your distro's package manager for playback to work."
fi

if [ ! -e /usr/share/mpv/scripts/mpris.so ] && [ ! -e /usr/lib/mpv/scripts/mpris.so ] && [ ! -e /usr/local/share/mpv/scripts/mpris.so ] && [ -z "${LOFI_MPV_MPRIS_SCRIPT:-}" ]; then
    echo "Note: mpv-mpris not detected, now-playing widgets (quickshell, playerctl, etc.) won't see this player. Playback still works without it."
fi

echo "Install complete. Open a new terminal to start playback automatically."
```

- [ ] **Step 3: Write `scripts/update.sh`**

```sh
#!/usr/bin/env sh
set -eu

REPO_DIR="$(cd "$(dirname "$0")/.." && pwd)"
PREFIX="${PREFIX:-$HOME/.local}"
BIN_DIR="$PREFIX/bin"

if [ -d "$REPO_DIR/.git" ]; then
    echo "Pulling latest source..."
    (cd "$REPO_DIR" && git pull)
fi

echo "Rebuilding release binaries..."
(cd "$REPO_DIR" && cargo build --release)

mkdir -p "$BIN_DIR"
cp "$REPO_DIR/target/release/lofi" "$BIN_DIR/lofi"
cp "$REPO_DIR/target/release/lofi-daemon" "$BIN_DIR/lofi-daemon"
echo "Updated binaries in $BIN_DIR. Existing config and shell integration are preserved."
```

- [ ] **Step 4: Write `scripts/uninstall.sh`**

```sh
#!/usr/bin/env sh
set -eu

PREFIX="${PREFIX:-$HOME/.local}"
BIN_DIR="$PREFIX/bin"
CONFIG_DIR="${XDG_CONFIG_HOME:-$HOME/.config}/lofi-launcher"
MARKER_START="# >>> lofi-launcher-terminal >>>"
MARKER_END="# <<< lofi-launcher-terminal <<<"
PURGE=0

for arg in "$@"; do
    if [ "$arg" = "--purge" ]; then
        PURGE=1
    fi
done

pkill -u "$(id -u)" -f 'lofi-daemon' 2>/dev/null || true

rm -f "$BIN_DIR/lofi" "$BIN_DIR/lofi-daemon"
echo "Removed binaries from $BIN_DIR"

for RC_FILE in "$HOME/.bashrc" "$HOME/.zshrc"; do
    if [ -f "$RC_FILE" ] && grep -qF "$MARKER_START" "$RC_FILE"; then
        sed -i "/$MARKER_START/,/$MARKER_END/d" "$RC_FILE"
        echo "Removed shell integration from $RC_FILE"
    fi
done

if [ "$PURGE" = "1" ]; then
    rm -rf "$CONFIG_DIR"
    echo "Removed config directory $CONFIG_DIR"
else
    echo "Config directory $CONFIG_DIR left in place; re-run with --purge to remove it."
fi
```

- [ ] **Step 5: Make the scripts executable**

Run: `chmod +x scripts/install.sh scripts/update.sh scripts/uninstall.sh`

- [ ] **Step 6: Manual verification**

Run `./scripts/install.sh` in a throwaway shell session, open a new terminal, confirm `lofi status` reports a running daemon, run `./scripts/uninstall.sh`, confirm the binaries and rc snippet are gone and the config directory remains, then `./scripts/uninstall.sh --purge` on a fresh install to confirm the config directory is also removed.

- [ ] **Step 7: Commit**

```bash
git add scripts
git commit -m "Add install, update, and uninstall scripts with shell integration"
```

---

### Task 9: Keyword classifier and `lofi add` command

**Files:**
- Create: `crates/lofi-common/src/classifier.rs`
- Modify: `crates/lofi-common/src/lib.rs`
- Modify: `crates/lofi-common/src/protocol.rs` (add the `Add` command variant)
- Modify: `crates/lofi-daemon/src/state.rs` (handle `Command::Add`)
- Modify: `crates/lofi-cli/src/main.rs` (add the `add` subcommand)
- Test: inline `#[cfg(test)]` modules in `classifier.rs` and `state.rs`

**Interfaces:**
- Consumes: `Config::classifier` (`BTreeMap<String, Vec<String>>`) from Task 1's `Config` struct.
- Produces: `pub fn classify(classifier: &std::collections::BTreeMap<String, Vec<String>>, title: &str, description: &str) -> Option<String>` in `lofi-common`. Extends `Command` with `Add { source: String, mood: Option<String> }` and `Response` with `Classified(String)` (the chosen mood name). `DaemonState::handle` grows a case for `Command::Add` that appends to the config's in-memory mood list and rewrites the config file.

- [ ] **Step 1: Write the failing classifier test**

In `crates/lofi-common/src/classifier.rs`:

```rust
use std::collections::BTreeMap;

pub fn classify(classifier: &BTreeMap<String, Vec<String>>, title: &str, description: &str) -> Option<String> {
    let haystack = format!("{title} {description}").to_lowercase();
    for (mood, keywords) in classifier {
        for keyword in keywords {
            if haystack.contains(&keyword.to_lowercase()) {
                return Some(mood.clone());
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_classifier() -> BTreeMap<String, Vec<String>> {
        let mut m = BTreeMap::new();
        m.insert("rainy-day".to_string(), vec!["rain".to_string(), "storm".to_string()]);
        m.insert("ambient".to_string(), vec!["ambient".to_string(), "drone".to_string()]);
        m
    }

    #[test]
    fn matches_keyword_in_title_case_insensitively() {
        let result = classify(&sample_classifier(), "Heavy RAIN sounds for sleep", "");
        assert_eq!(result, Some("rainy-day".to_string()));
    }

    #[test]
    fn matches_keyword_in_description_when_title_has_none() {
        let result = classify(&sample_classifier(), "3 hour mix", "a deep ambient drone soundscape");
        assert_eq!(result, Some("ambient".to_string()));
    }

    #[test]
    fn returns_none_when_nothing_matches() {
        let result = classify(&sample_classifier(), "Upbeat pop music", "dance tracks");
        assert_eq!(result, None);
    }
}
```

- [ ] **Step 2: Run test to verify it fails, then wire the module in, then verify it passes**

Run: `cargo test -p lofi-common classifier`
Expected: FAIL to compile first (module not registered). Add `pub mod classifier;` and `pub use classifier::classify;` to `crates/lofi-common/src/lib.rs`, then run again.
Expected: PASS, all three tests green.

- [ ] **Step 3: Extend the protocol with `Add` and `Classified`**

In `crates/lofi-common/src/protocol.rs`, add a variant to each enum:

```rust
pub enum Command {
    Register,
    Unregister,
    Mood(String),
    Next,
    Pause,
    Resume,
    Status,
    Moods,
    Reload,
    Add { source: String, mood: Option<String> },
}
```

```rust
pub enum Response {
    Ok,
    Error(String),
    Status {
        mood: String,
        playing: bool,
        current_source: Option<String>,
    },
    Moods(Vec<String>),
    Classified(String),
}
```

Run: `cargo test -p lofi-common`
Expected: PASS, existing round-trip tests from Task 2 still cover `Command`/`Response` serde derives generically and keep passing unchanged.

- [ ] **Step 4: Write the failing test for daemon-side `Add` handling**

Add to the `tests` module in `crates/lofi-daemon/src/state.rs` (reusing the `FakeMpv` and `test_config` helpers already defined there):

```rust
#[test]
fn add_with_explicit_mood_appends_source_and_reports_classified_mood() {
    let mut state = DaemonState::new(test_config(), FakeMpv::default());
    let resp = state.handle(Command::Add {
        source: "https://example.com/mix.mp4".to_string(),
        mood: Some("deep-focus".to_string()),
    });
    match resp {
        Response::Classified(mood) => assert_eq!(mood, "deep-focus"),
        other => panic!("expected Classified, got {other:?}"),
    }
    let resp = state.handle(Command::Status);
    let _ = resp;
    assert!(state.mood_sources("deep-focus").contains(&"https://example.com/mix.mp4".to_string()));
}

#[test]
fn add_with_unknown_explicit_mood_returns_error() {
    let mut state = DaemonState::new(test_config(), FakeMpv::default());
    let resp = state.handle(Command::Add {
        source: "a.mp3".to_string(),
        mood: Some("not-a-mood".to_string()),
    });
    assert!(matches!(resp, Response::Error(_)));
}
```

- [ ] **Step 5: Run test to verify it fails**

Run: `cargo test -p lofi-daemon state`
Expected: FAIL to compile, `Command::Add` not handled in `handle`'s match, and `mood_sources` not defined.

- [ ] **Step 6: Implement `Command::Add` handling in `state.rs`**

Add a `pub fn mood_sources(&self, mood: &str) -> Vec<String>` accessor and extend the `handle` match:

```rust
pub fn mood_sources(&self, mood: &str) -> Vec<String> {
    self.config.moods.get(mood).map(|m| m.sources.clone()).unwrap_or_default()
}
```

```rust
Command::Add { source, mood } => {
    let target_mood = match mood {
        Some(name) => {
            if !self.config.moods.contains_key(&name) {
                return Response::Error(format!(
                    "unknown mood '{name}', valid moods: {}",
                    self.valid_mood_names()
                ));
            }
            name
        }
        None => {
            return Response::Error(
                "could not classify source without metadata; pass --mood explicitly \
                 (classification from a fetched title/description happens in the CLI \
                 before this command is sent)".to_string(),
            );
        }
    };
    self.config
        .moods
        .get_mut(&target_mood)
        .expect("checked above")
        .sources
        .push(source);
    if let Err(e) = lofi_common::save_config(&lofi_common::config_path(), &self.config) {
        return Response::Error(e.to_string());
    }
    Response::Classified(target_mood)
}
```

This daemon-side `Add` always requires a resolved mood name. The CLI (Step 8 below) is responsible for running `yt-dlp` and calling `classify` before sending the command, so the daemon itself never shells out to `yt-dlp` and stays easy to unit test.

- [ ] **Step 7: Add `save_config` to `lofi-common` and run tests**

In `crates/lofi-common/src/config.rs`, add:

```rust
pub fn save_config(path: &std::path::Path, config: &Config) -> anyhow::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let text = toml::to_string_pretty(config)?;
    std::fs::write(path, text)?;
    Ok(())
}
```

Re-export it from `lib.rs` alongside `load_config`.

Run: `cargo test -p lofi-common -p lofi-daemon`
Expected: PASS, including the two new `Add`-handling tests.

- [ ] **Step 8: Add the `lofi add` CLI subcommand**

In `crates/lofi-cli/src/main.rs`, extend the `Cmd` enum:

```rust
Add {
    source: String,
    #[arg(long)]
    mood: Option<String>,
},
```

And extend `main`'s dispatch, before the generic `cmd` match that calls `send_command`:

```rust
if let Cmd::Add { source, mood } = &cli.command {
    client::ensure_daemon_running(&socket)?;
    let resolved_mood = match mood {
        Some(m) => Some(m.clone()),
        None => client::classify_source(source)?,
    };
    if resolved_mood.is_none() {
        eprintln!(
            "could not classify '{source}' into a mood automatically; re-run with --mood <name>"
        );
        std::process::exit(1);
    }
    let resp = client::send_command(
        &socket,
        &DaemonCommand::Add { source: source.clone(), mood: resolved_mood },
    )?;
    print_response(resp);
    return Ok(());
}
```

Add `pub fn classify_source(source: &str) -> anyhow::Result<Option<String>>` to `crates/lofi-cli/src/client.rs`:

```rust
pub fn classify_source(source: &str) -> anyhow::Result<Option<String>> {
    let is_local = std::path::Path::new(source).exists();
    if is_local {
        return Ok(None);
    }
    let yt_dlp_bin = std::env::var("LOFI_YTDLP_BIN").unwrap_or_else(|_| "yt-dlp".to_string());
    let output = std::process::Command::new(&yt_dlp_bin)
        .arg("--dump-json")
        .arg("--skip-download")
        .arg(source)
        .output();
    let output = match output {
        Ok(o) if o.status.success() => o,
        _ => {
            eprintln!("yt-dlp not detected or failed to fetch metadata for '{source}'");
            return Ok(None);
        }
    };
    let json: serde_json::Value = serde_json::from_slice(&output.stdout)?;
    let title = json.get("title").and_then(|v| v.as_str()).unwrap_or("");
    let description = json.get("description").and_then(|v| v.as_str()).unwrap_or("");

    let config_path = lofi_common::config_path();
    let config = lofi_common::load_config(&config_path)?;
    Ok(lofi_common::classify(&config.classifier, title, description))
}
```

Add `serde_json = { workspace = true }` to `crates/lofi-cli/Cargo.toml` under `[dependencies]` if not already present from an earlier task.

- [ ] **Step 9: Run the full workspace build and tests**

Run: `cargo build && cargo test`
Expected: clean build, all tests pass across `lofi-common`, `lofi-daemon`, `lofi-cli`.

- [ ] **Step 10: Manual check**

With a daemon running and `yt-dlp` installed, run `lofi add <a real YouTube lofi mix URL>` and confirm it prints the classified mood and the source shows up in `~/.config/lofi-launcher/config.toml`. Run `lofi add ./some-local-file.mp3` and confirm it requires `--mood` since local files carry no fetchable title/description.

- [ ] **Step 11: Commit**

```bash
git add crates/lofi-common/src/classifier.rs crates/lofi-common/src/lib.rs crates/lofi-common/src/protocol.rs crates/lofi-common/src/config.rs crates/lofi-daemon/src/state.rs crates/lofi-cli/src/main.rs crates/lofi-cli/src/client.rs crates/lofi-cli/Cargo.toml
git commit -m "Add keyword classifier and lofi add command"
```

---

### Task 10: Random-seek playback for long sources

**Files:**
- Modify: `crates/lofi-daemon/src/mpv.rs` (`MpvController` trait and `RealMpv` impl)
- Modify: `crates/lofi-daemon/src/state.rs` (pass duration/threshold through to the controller)
- Test: inline `#[cfg(test)]` additions in `state.rs`

**Interfaces:**
- Consumes: `Config::long_source_minutes` from Task 1.
- Produces: extends `MpvController` with `fn start_source_with_duration(&mut self, source: &str, duration_seconds: Option<u64>, long_source_threshold_seconds: u64) -> anyhow::Result<()>`, which replaces direct calls to `start_source` from `DaemonState::start_current_mood`. `start_source` stays on the trait for the simple case and is called internally when no duration is known or the source is short.

- [ ] **Step 1: Write the failing test using the existing `FakeMpv`**

Extend `FakeMpv` in `crates/lofi-daemon/src/state.rs`'s test module to record whether a seek was requested, and add the `MpvController` method to its impl:

```rust
#[derive(Default)]
struct FakeMpv {
    started: Vec<String>,
    stopped: bool,
    paused: bool,
    last_seek_requested: bool,
}

impl MpvController for FakeMpv {
    // existing methods unchanged, plus:
    fn start_source_with_duration(
        &mut self,
        source: &str,
        duration_seconds: Option<u64>,
        long_source_threshold_seconds: u64,
    ) -> anyhow::Result<()> {
        self.started.push(source.to_string());
        self.stopped = false;
        self.last_seek_requested = matches!(duration_seconds, Some(d) if d > long_source_threshold_seconds);
        Ok(())
    }
}
```

```rust
#[test]
fn long_source_above_threshold_requests_a_seek() {
    let mut moods = BTreeMap::new();
    moods.insert(
        "ambient".to_string(),
        Mood { sources: vec!["https://example.com/3hour-mix.mp4".to_string()] },
    );
    let config = Config {
        default_mood: "ambient".to_string(),
        moods,
        classifier: BTreeMap::new(),
        long_source_minutes: 20,
    };
    let mut state = DaemonState::new(config, FakeMpv::default());
    state.set_known_duration_seconds_for_test(Some(3 * 3600));
    state.handle(Command::Register);
    assert!(state.mpv.last_seek_requested, "a 3 hour source must request a random seek");
}

#[test]
fn short_source_does_not_request_a_seek() {
    let mut moods = BTreeMap::new();
    moods.insert(
        "ambient".to_string(),
        Mood { sources: vec!["short-track.mp3".to_string()] },
    );
    let config = Config {
        default_mood: "ambient".to_string(),
        moods,
        classifier: BTreeMap::new(),
        long_source_minutes: 20,
    };
    let mut state = DaemonState::new(config, FakeMpv::default());
    state.set_known_duration_seconds_for_test(Some(180));
    state.handle(Command::Register);
    assert!(!state.mpv.last_seek_requested, "a 3 minute source must not request a seek");
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p lofi-daemon state`
Expected: FAIL to compile, `start_source_with_duration` and `set_known_duration_seconds_for_test` not defined.

- [ ] **Step 3: Extend `MpvController` in `mpv.rs`**

```rust
pub trait MpvController {
    fn start_source(&mut self, source: &str) -> anyhow::Result<()>;
    fn start_source_with_duration(
        &mut self,
        source: &str,
        duration_seconds: Option<u64>,
        long_source_threshold_seconds: u64,
    ) -> anyhow::Result<()>;
    fn stop(&mut self) -> anyhow::Result<()>;
    fn pause(&mut self) -> anyhow::Result<()>;
    fn resume(&mut self) -> anyhow::Result<()>;
    fn last_error(&self) -> Option<String>;
}
```

Implement the default-friendly real version on `RealMpv` (the fake in tests implements it directly, shown in Step 1):

```rust
impl MpvController for RealMpv {
    fn start_source(&mut self, source: &str) -> anyhow::Result<()> {
        self.start_source_with_duration(source, None, u64::MAX)
    }

    fn start_source_with_duration(
        &mut self,
        source: &str,
        duration_seconds: Option<u64>,
        long_source_threshold_seconds: u64,
    ) -> anyhow::Result<()> {
        self.send(serde_json::json!({ "command": ["loadfile", source, "replace"] }))?;
        if let Some(duration) = duration_seconds {
            if duration > long_source_threshold_seconds {
                let offset = rand_offset_seconds(duration);
                self.send(serde_json::json!({ "command": ["set_property", "time-pos", offset] }))?;
            }
        }
        self.send(serde_json::json!({ "command": ["set_property", "loop-file", "inf"] }))?;
        Ok(())
    }

    // stop, pause, resume, last_error unchanged from Task 4
}

fn rand_offset_seconds(duration_seconds: u64) -> u64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    let nanos = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().subsec_nanos() as u64;
    let usable_range = duration_seconds.saturating_sub(60).max(1);
    nanos % usable_range
}
```

`rand_offset_seconds` avoids pulling in a random-number crate: `mpv`'s own IPC call happens at most once per `register`/`mood`/`next`, so a nanosecond-timestamp-derived offset is varied enough for this use case without adding a dependency. It leaves a 60 second margin at the end of the file so a seek never lands past the point where looping would immediately kick in.

- [ ] **Step 4: Wire the threshold through `DaemonState`**

In `state.rs`, add a field and a test-only setter, and use it in `start_current_mood`:

```rust
pub struct DaemonState<M: MpvController> {
    config: Config,
    pub mpv: M,
    session_count: u32,
    current_mood: String,
    current_index: usize,
    playing: bool,
    known_duration_seconds: Option<u64>,
}
```

Update `DaemonState::new` to initialize `known_duration_seconds: None`, and update `start_current_mood`:

```rust
fn start_current_mood(&mut self) -> Response {
    let mood = match self.config.moods.get(&self.current_mood) {
        Some(m) => m,
        None => return Response::Error(format!("unknown mood: {}", self.current_mood)),
    };
    match mood.sources.get(self.current_index) {
        Some(source) => {
            let threshold_seconds = (self.config.long_source_minutes as u64) * 60;
            let result = self.mpv.start_source_with_duration(
                source,
                self.known_duration_seconds,
                threshold_seconds,
            );
            match result {
                Ok(()) => {
                    self.playing = true;
                    Response::Ok
                }
                Err(e) => Response::Error(e.to_string()),
            }
        }
        None => Response::Error(format!("mood '{}' has no sources configured", self.current_mood)),
    }
}

#[cfg(test)]
pub fn set_known_duration_seconds_for_test(&mut self, seconds: Option<u64>) {
    self.known_duration_seconds = seconds;
}
```

In real daemon operation (outside tests), `known_duration_seconds` is populated by the daemon reading a `duration_seconds` field cached on `Mood`'s source entries when `lofi add` first classified them (a natural follow-up refinement; recording this cache is out of scope for this task, which only wires the threshold decision and the seek call end to end for sources whose duration is already known).

- [ ] **Step 5: Run tests to verify they pass**

Run: `cargo test -p lofi-daemon`
Expected: PASS, including the two new long-source tests and everything from Tasks 3 to 9.

- [ ] **Step 6: Commit**

```bash
git add crates/lofi-daemon/src/mpv.rs crates/lofi-daemon/src/state.rs
git commit -m "Add random-seek playback for long sources"
```

---

### Task 11: PKGBUILD for Arch-based distributions

**Files:**
- Create: `PKGBUILD`

**Interfaces:**
- Consumes: the workspace `Cargo.toml`/`Cargo.lock`, `config.default.toml`, `scripts/lofi-launcher.sh.in`.
- Produces: an Arch package installing `lofi` and `lofi-daemon` to `/usr/bin`, with `mpv` declared as a runtime dependency and `mpv-mpris` declared as an optional dependency.

- [ ] **Step 1: Write `PKGBUILD`**

```bash
# Maintainer: dollamike123 <dollamike123@gmail.com>
pkgname=lofi-launcher-terminal
pkgver=0.1.0
pkgrel=1
pkgdesc="Play mood-based lofi music in the background whenever a terminal or TTY session is open"
arch=('x86_64' 'aarch64')
url="https://github.com/dollamike123/lofi-launcher-terminal"
license=('MIT')
depends=('mpv')
optdepends=('mpv-mpris: expose now-playing track info over MPRIS for widgets like quickshell or playerctl')
makedepends=('cargo')
source=("$pkgname-$pkgver.tar.gz::https://github.com/dollamike123/lofi-launcher-terminal/archive/v$pkgver.tar.gz")
sha256sums=('SKIP')

build() {
    cd "$pkgname-$pkgver"
    cargo build --release --locked
}

package() {
    cd "$pkgname-$pkgver"
    install -Dm755 target/release/lofi "$pkgdir/usr/bin/lofi"
    install -Dm755 target/release/lofi-daemon "$pkgdir/usr/bin/lofi-daemon"
    install -Dm644 config.default.toml "$pkgdir/usr/share/lofi-launcher-terminal/config.default.toml"
    install -Dm644 scripts/lofi-launcher.sh.in "$pkgdir/usr/share/lofi-launcher-terminal/lofi-launcher.sh.in"
}
```

- [ ] **Step 2: Verify with `namcap` or a local build, if running on Arch**

Run: `makepkg --printsrcinfo > .SRCINFO` to generate the companion metadata file Arch repositories expect, and `makepkg -s` on an Arch machine to confirm it builds, if available. On a non-Arch dev machine, skip this step and note it as manually verified later on Arch hardware.

- [ ] **Step 3: Commit**

```bash
git add PKGBUILD .SRCINFO
git commit -m "Add PKGBUILD for Arch-based distributions"
```

(Omit `.SRCINFO` from the commit if `makepkg` was unavailable to generate it; generate and commit it in a follow-up once tested on Arch.)

---

### Task 12: README and final workspace check

**Files:**
- Create: `README.md`
- Create: `.gitignore`

**Interfaces:**
- Consumes: nothing; this is documentation and repo hygiene.
- Produces: nothing consumed by other tasks; this is the last task in the plan.

- [ ] **Step 1: Create `.gitignore`**

```
/target
```

- [ ] **Step 2: Write `README.md`**

```markdown
# lofi-launcher-terminal

Plays mood-based lofi music in the background for as long as any terminal
or TTY session is open on the machine.

## How it works

Opening a terminal or logging into a TTY runs `lofi register`, which starts
a shared background daemon on first use and starts playback through `mpv`.
Closing the session runs `lofi unregister`. Music keeps playing as long as
at least one session is open, and stops when the last one closes.

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

Edit `~/.config/lofi-launcher/config.toml`. Each of the four built-in moods
(`code-and-chill`, `deep-focus`, `chill-beats`, `rainy-day`) takes a list of
sources, where a source is a local file/directory path or any URL mpv's
built-in ytdl hook can resolve. Synthwave, retrowave, and vaporwave are
genre flavors: mix them into whichever mood's list fits, rather than adding
new mood keys.

## CLI

```sh
lofi status           # show current mood, playing/paused, current source
lofi mood deep-focus   # switch mood
lofi next              # skip to the next source in the current mood
lofi pause / lofi resume
lofi moods             # list configured mood names
lofi tui               # interactive mood picker
```

## Requirements

Linux, `mpv` installed and on `PATH` (or pointed to via `LOFI_MPV_BIN`).

## Now-playing widgets (quickshell, playerctl, waybar)

Install `mpv-mpris` (available in most distro repos, or as an optdepend on Arch) so mpv publishes track title and play/pause state over MPRIS. Any standard MPRIS-reading widget then sees and can control the current lofi track. This is optional: playback works the same without it, only widget visibility is affected. If your `mpv-mpris` script lives somewhere nonstandard, point to it with `LOFI_MPV_MPRIS_SCRIPT=/path/to/mpris.so`.
```

- [ ] **Step 3: Run the full workspace test suite one last time**

Run: `cargo test`
Expected: PASS across `lofi-common`, `lofi-daemon`, and `lofi-cli`.

- [ ] **Step 4: Commit**

```bash
git add README.md .gitignore
git commit -m "Add README and gitignore"
```

---

## Self-Review Notes

**Spec coverage:** Two-binary daemon/CLI split (Tasks 3 to 6), mpv-backed playback (Task 4), Unix socket IPC (Tasks 2, 5), refcounted session lifecycle (Task 3), config-driven moods with local/URL sources (Task 1), five built-in moods including ambient (Task 1), synthwave/retrowave/vaporwave as genre flavors documented in config and README (Tasks 1, 12), shell integration for any terminal/TTY (Task 8), CLI mood switching plus optional TUI (Tasks 6 to 7), `lofi add` with keyword classification (Task 9), long-source random-seek playback (Task 10), install/update/uninstall scripts (Task 8), PKGBUILD tracked in git (Task 11), missing-mpv and missing-yt-dlp handling (Tasks 5 to 6, 8 to 9), XDG/PREFIX path resolution (Tasks 1, 5 to 6, 8), unit and manual end-to-end testing (all tasks plus Task 8 Step 6, Task 9 Step 10). Spotify is intentionally not covered by any task, per the spec's "Future work" section.

**Placeholder scan:** The only intentional placeholders are the Task 6 Step 7 `tui.rs` stub and the Task 5 Step 7 `lofi-cli` scaffold stub, both explicitly temporary and replaced by name in a later numbered task (Task 7, Task 6), not left as open-ended TODOs. Task 10's note that duration caching on `Mood` entries is "a natural follow-up refinement" is a deliberate scope boundary stated in the spec's random-seek design (seek-on-known-duration, not a duration-fetching pipeline), not an unaddressed requirement; `known_duration_seconds` is exercised via the test-only setter until a source's duration is captured at `lofi add` time.

**Type consistency:** `Command`/`Response` variants introduced in Task 2 and extended in Task 9 (`Add`, `Classified`) are used identically in Tasks 3, 5, 6, 7, and 9 (`Register`, `Unregister`, `Mood(String)`, `Next`, `Pause`, `Resume`, `Status`, `Moods`, `Reload`, `Add { source, mood }`, and the matching `Response` variants). `MpvController` trait methods defined in Task 3 (`start_source`, `stop`, `pause`, `resume`, `last_error`) and extended in Task 10 (`start_source_with_duration`) are implemented identically by `RealMpv` in Tasks 4 and 10 and by the test `FakeMpv` in Tasks 3 and 10. `DaemonState::handle` and `session_count` signatures from Task 3 are consumed unchanged in Task 5's `server.rs`. `Config`'s `classifier` and `long_source_minutes` fields added in this revision of Task 1 are consumed unchanged by Task 9's `classify` call and Task 10's `start_current_mood`.

**Review Focus coverage:** missing config file (Task 5 Step 5, `main.rs` falls back to `default_config_toml()`), stale daemon socket (Task 6 Step 4, `ensure_daemon_running` removes and respawns), refcount double-register/over-unregister (Task 3 Steps 1 and 5, `register_starts_playback_only_on_first_session` and `unregister_below_zero_is_a_no_op`), unknown mood name (Task 3, `mood_switch_to_unknown_mood_returns_error_listing_valid_names`), mpv error surfacing on a bad source (partially covered by `mood_with_empty_sources_reports_warning_instead_of_starting` for the empty-list case; a genuinely bad-but-present URL/path is passed through to mpv's own error reporting via `last_error`, left as a manual check in Task 4 Step 5 rather than a unit test, since it requires a real mpv process to observe mpv's own error behavior), `lofi add` with no classifier match (Task 9, the CLI-side "could not classify" exit-1 path in Step 8, verified manually in Step 10), `lofi add` on a local file or with `yt-dlp` missing (Task 9's `classify_source`, which returns `Ok(None)` for a local path or a failed `yt-dlp` invocation rather than erroring).
