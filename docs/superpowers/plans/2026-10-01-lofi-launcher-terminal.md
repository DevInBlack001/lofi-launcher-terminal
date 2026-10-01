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
- Four built-in moods, exact keys: `code-and-chill`, `deep-focus`, `chill-beats`, `rainy-day`. Synthwave/retrowave/vaporwave are documented as genre flavors to mix into these four, never added as separate mood keys.
- A mood with an empty `sources` list must warn clearly, never panic or fail silently.

## Review Focus

- Config file missing entirely on first run: daemon/CLI must create a default template rather than erroring, since `scripts/install.sh` may not have run first (e.g. a dev build).
- Daemon socket left behind by a crashed daemon (stale socket file, no listener): CLI must detect connect failure, remove the stale socket, and respawn, rather than hanging on connect.
- `register` called many times in quick succession (e.g. several terminal tabs opening at once) or `unregister` called when the count is already 0: refcount must never go negative and must stay consistent under this ordering.
- `mood <name>` given a name not present in the config: CLI/daemon must report an actionable "unknown mood" error listing valid names, not a panic or a silent no-op.
- `mpv` present but a configured source is an unreachable/invalid URL or missing local path: the daemon must surface the mpv error via `status` rather than the process or socket dying, and must stay able to accept the next command (e.g. switch mood away from the bad source).

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
- Produces: `pub struct Config { pub default_mood: String, pub moods: std::collections::BTreeMap<String, Mood> }`, `pub struct Mood { pub sources: Vec<String> }`, `pub fn config_path() -> std::path::PathBuf`, `pub fn load_config(path: &std::path::Path) -> anyhow::Result<Config>`, `pub fn default_config_toml() -> &'static str`, `pub const BUILTIN_MOODS: [&str; 4] = ["code-and-chill", "deep-focus", "chill-beats", "rainy-day"];`

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

pub const BUILTIN_MOODS: [&str; 4] = ["code-and-chill", "deep-focus", "chill-beats", "rainy-day"];

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Mood {
    #[serde(default)]
    pub sources: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Config {
    pub default_mood: String,
    pub moods: BTreeMap<String, Mood>,
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
        Config { default_mood: "code-and-chill".to_string(), moods }
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
- Produces: `pub struct RealMpv { socket_path: std::path::PathBuf, child: std::process::Child }` with `pub fn spawn(socket_path: std::path::PathBuf) -> anyhow::Result<Self>` and the `MpvController` impl.

- [ ] **Step 1: Write a test that is skipped gracefully when mpv is not installed**

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

impl RealMpv {
    pub fn spawn(socket_path: PathBuf) -> anyhow::Result<Self> {
        let ipc_arg = format!("--input-ipc-server={}", socket_path.display());
        let child = Command::new(mpv_binary())
            .arg("--idle")
            .arg("--no-video")
            .arg(ipc_arg)
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
        let config = Config { default_mood: "code-and-chill".to_string(), moods };
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

### Task 9: PKGBUILD for Arch-based distributions

**Files:**
- Create: `PKGBUILD`

**Interfaces:**
- Consumes: the workspace `Cargo.toml`/`Cargo.lock`, `config.default.toml`, `scripts/lofi-launcher.sh.in`.
- Produces: an Arch package installing `lofi` and `lofi-daemon` to `/usr/bin`, with `mpv` declared as a runtime dependency.

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

### Task 10: README and final workspace check

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

**Spec coverage:** Two-binary daemon/CLI split (Tasks 3 to 6), mpv-backed playback (Task 4), Unix socket IPC (Tasks 2, 5), refcounted session lifecycle (Task 3), config-driven moods with local/URL sources (Task 1), synthwave/retrowave/vaporwave as genre flavors documented in config and README (Tasks 1, 10), shell integration for any terminal/TTY (Task 8), CLI mood switching plus optional TUI (Tasks 6 to 7), install/update/uninstall scripts (Task 8), PKGBUILD tracked in git (Task 9), missing-mpv handling (Tasks 5 to 6, 8), XDG/PREFIX path resolution (Tasks 1, 5 to 6, 8), unit and manual end-to-end testing (all tasks plus Task 8 Step 6).

**Placeholder scan:** The only intentional placeholders are the Task 6 Step 7 `tui.rs` stub and the Task 5 Step 7 `lofi-cli` scaffold stub, both explicitly temporary and replaced by name in a later numbered task (Task 7, Task 6), not left as open-ended TODOs.

**Type consistency:** `Command`/`Response` variants introduced in Task 2 are used identically in Tasks 3, 5, 6, and 7 (`Register`, `Unregister`, `Mood(String)`, `Next`, `Pause`, `Resume`, `Status`, `Moods`, `Reload`, and the matching `Response` variants). `MpvController` trait methods defined in Task 3 (`start_source`, `stop`, `pause`, `resume`, `last_error`) are implemented identically by `RealMpv` in Task 4. `DaemonState::handle` and `session_count` signatures from Task 3 are consumed unchanged in Task 5's `server.rs`.

**Review Focus coverage:** missing config file (Task 5 Step 5, `main.rs` falls back to `default_config_toml()`), stale daemon socket (Task 6 Step 4, `ensure_daemon_running` removes and respawns), refcount double-register/over-unregister (Task 3 Steps 1 and 5, `register_starts_playback_only_on_first_session` and `unregister_below_zero_is_a_no_op`), unknown mood name (Task 3, `mood_switch_to_unknown_mood_returns_error_listing_valid_names`), mpv error surfacing on a bad source (partially covered by `mood_with_empty_sources_reports_warning_instead_of_starting` for the empty-list case; a genuinely bad-but-present URL/path is passed through to mpv's own error reporting via `last_error`, left as a manual check in Task 4 Step 5 rather than a unit test, since it requires a real mpv process to observe mpv's own error behavior).
