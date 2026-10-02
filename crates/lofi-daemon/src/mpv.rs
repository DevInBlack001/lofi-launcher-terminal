use std::io::{BufRead, BufReader, Write};
use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

#[derive(Debug, Clone, Copy)]
pub struct StartOptions<'a> {
    pub known_duration_seconds: Option<u64>,
    pub long_source_threshold_seconds: u64,
    // yt-dlp format selector for mpv's ytdl hook. mpv only consults it for
    // sources the hook resolves (URLs); local files ignore it entirely.
    pub ytdl_format: &'a str,
    // Where to begin playback (a chapter the user picked). When set, the
    // random seek into long sources is skipped: the user chose the position.
    pub seek_seconds: Option<u64>,
}

pub trait MpvController {
    // Convenience wrapper that RealMpv's own start_source delegates through for the
    // simple case; nothing in the current binary calls it directly since everything
    // goes through start_source_with_options now.
    #[allow(dead_code)]
    fn start_source(&mut self, source: &str) -> anyhow::Result<()>;
    fn start_source_with_options(&mut self, source: &str, options: &StartOptions) -> anyhow::Result<()>;
    fn stop(&mut self) -> anyhow::Result<()>;
    fn pause(&mut self) -> anyhow::Result<()>;
    fn resume(&mut self) -> anyhow::Result<()>;
    // Part of the trait contract for future error-surfacing use; not yet read anywhere.
    #[allow(dead_code)]
    fn last_error(&self) -> Option<String>;
    fn quit(&mut self) -> anyhow::Result<()>;
}

pub struct RealMpv {
    socket_path: PathBuf,
    child: Child,
    // Bumped on every source change so background work for an earlier source
    // (the duration probe, the end-of-file listener) never acts on whatever is
    // playing now.
    generation: Arc<AtomicU64>,
}

const DEFAULT_IPC_TIMEOUT: Duration = Duration::from_secs(2);
// Network sources resolve through yt-dlp, which routinely takes several
// seconds. The probe runs off the daemon's lock, so it can afford to wait.
const DURATION_PROBE_BUDGET: Duration = Duration::from_secs(30);
const DURATION_PROBE_ATTEMPT_TIMEOUT: Duration = Duration::from_millis(500);
const DURATION_PROBE_INTERVAL: Duration = Duration::from_millis(250);
// A single IPC reply is one short JSON line; anything far larger is not mpv.
const MAX_IPC_LINE_BYTES: u64 = 64 * 1024;

fn ipc_request(socket_path: &Path, payload: &serde_json::Value, read_timeout: Duration) -> anyhow::Result<serde_json::Value> {
    use std::io::Read;
    let mut stream = UnixStream::connect(socket_path)?;
    let mut line = serde_json::to_string(payload)?;
    line.push('\n');
    stream.write_all(line.as_bytes())?;
    stream.set_read_timeout(Some(read_timeout))?;
    let mut reader = BufReader::new(stream);
    let deadline = Instant::now() + read_timeout;
    // mpv broadcasts events to every client, so the first line on a fresh
    // connection may be an event rather than the reply to this request.
    loop {
        let mut response = String::new();
        let read = (&mut reader).take(MAX_IPC_LINE_BYTES).read_line(&mut response)?;
        if read == 0 {
            anyhow::bail!("mpv closed the IPC connection without replying");
        }
        let value: serde_json::Value = serde_json::from_str(&response)?;
        if value.get("event").is_none() {
            return Ok(value);
        }
        if Instant::now() > deadline {
            anyhow::bail!("timed out waiting for mpv's reply");
        }
    }
}

// Gives up after `budget`; a source whose duration never appears (e.g. a live
// broadcast or an internet radio stream) is then simply never seeked.
fn query_duration(socket_path: &Path, generation: &AtomicU64, expected_generation: u64, budget: Duration) -> Option<u64> {
    let deadline = Instant::now() + budget;
    while Instant::now() < deadline {
        // Give mpv a moment to unload the previous file first, so its duration
        // is not mistaken for the new one's.
        std::thread::sleep(DURATION_PROBE_INTERVAL);
        if generation.load(Ordering::SeqCst) != expected_generation {
            return None;
        }
        let request = serde_json::json!({ "command": ["get_property", "duration"] });
        if let Ok(resp) = ipc_request(socket_path, &request, DURATION_PROBE_ATTEMPT_TIMEOUT) {
            if let Some(secs) = resp.get("data").and_then(|d| d.as_f64()) {
                if secs > 0.0 {
                    return Some(secs as u64);
                }
            }
        }
    }
    None
}

fn seek_into_long_source(
    socket_path: &Path,
    generation: &AtomicU64,
    expected_generation: u64,
    known_duration_seconds: Option<u64>,
    long_source_threshold_seconds: u64,
) {
    let duration = match known_duration_seconds {
        Some(d) => d,
        None => match query_duration(socket_path, generation, expected_generation, DURATION_PROBE_BUDGET) {
            Some(d) => d,
            None => return,
        },
    };
    if duration <= long_source_threshold_seconds {
        return;
    }
    if !random_seek_makes_sense(socket_path) {
        return;
    }
    if generation.load(Ordering::SeqCst) != expected_generation {
        return;
    }
    let offset = rand_offset_seconds(duration);
    let seek = serde_json::json!({ "command": ["set_property", "time-pos", offset] });
    let _ = ipc_request(socket_path, &seek, DEFAULT_IPC_TIMEOUT);
}

// A live broadcast reports the time since it started as an ever-growing
// duration and claims to be seekable, but jumping to a random point in it
// would land far behind the live edge. mpv's ytdl hook tags such sources.
fn random_seek_makes_sense(socket_path: &Path) -> bool {
    let get = |property: &str| {
        ipc_request(
            socket_path,
            &serde_json::json!({ "command": ["get_property", property] }),
            DEFAULT_IPC_TIMEOUT,
        )
        .ok()
        .and_then(|reply| reply.get("data").cloned())
    };
    let is_live = get("metadata/by-key/ytdl_is_live").is_some_and(|v| v.as_str() == Some("true"));
    let unseekable = get("seekable").is_some_and(|v| v.as_bool() == Some(false));
    !is_live && !unseekable
}

fn mpv_binary() -> String {
    std::env::var("LOFI_MPV_BIN").unwrap_or_else(|_| "mpv".to_string())
}

const MPRIS_SCRIPT_CANDIDATES: [&str; 5] = [
    "/usr/share/mpv/scripts/mpris.so",
    "/usr/lib/mpv/scripts/mpris.so",
    "/usr/local/share/mpv/scripts/mpris.so",
    "/usr/lib/mpv-mpris/mpris.so",
    "/etc/mpv/scripts/mpris.so",
];

// mpv also auto-loads scripts from the user's own config directory, which can't
// be a static candidate since it depends on $HOME/$XDG_CONFIG_HOME at runtime.
fn user_mpv_script_dir_candidate() -> Option<PathBuf> {
    let base = lofi_common::xdg_absolute_dir("XDG_CONFIG_HOME")
        .or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".config")))?;
    Some(base.join("mpv").join("scripts").join("mpris.so"))
}

fn find_mpris_script() -> Option<String> {
    if let Ok(path) = std::env::var("LOFI_MPV_MPRIS_SCRIPT") {
        if std::path::Path::new(&path).exists() {
            return Some(path);
        }
        eprintln!("LOFI_MPV_MPRIS_SCRIPT is set to '{path}' but that file does not exist, ignoring");
    }
    if let Some(found) = MPRIS_SCRIPT_CANDIDATES
        .iter()
        .find(|p| std::path::Path::new(p).exists())
        .map(|p| p.to_string())
    {
        return Some(found);
    }
    user_mpv_script_dir_candidate()
        .filter(|p| p.exists())
        .map(|p| p.to_string_lossy().to_string())
}

impl RealMpv {
    pub fn spawn(socket_path: PathBuf) -> anyhow::Result<Self> {
        // A leftover socket from a dead mpv would make the readiness check below
        // pass before the new mpv has bound anything.
        if std::fs::symlink_metadata(&socket_path).is_ok() {
            std::fs::remove_file(&socket_path)?;
        }
        let ipc_arg = format!("--input-ipc-server={}", socket_path.display());
        let mut command = Command::new(mpv_binary());
        command
            .arg("--idle")
            .arg("--no-video")
            .arg("--keep-open=no")
            .arg("--loop-playlist=no")
            .arg(ipc_arg);
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
        // Constructed before the wait so an early return still kills mpv via Drop.
        let mut mpv = Self { socket_path, child, generation: Arc::new(AtomicU64::new(0)) };

        let deadline = Instant::now() + Duration::from_secs(3);
        loop {
            if UnixStream::connect(&mpv.socket_path).is_ok() {
                break;
            }
            if let Some(status) = mpv.child.try_wait()? {
                anyhow::bail!("mpv exited during startup ({status})");
            }
            if Instant::now() > deadline {
                anyhow::bail!("mpv did not accept IPC connections within 3 seconds");
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        // Defense in depth beyond $XDG_RUNTIME_DIR's own 0700: this socket accepts
        // mpv's "run" command, so nobody but the owner may connect.
        std::fs::set_permissions(&mpv.socket_path, std::fs::Permissions::from_mode(0o600))?;

        Ok(mpv)
    }

    fn send(&self, payload: serde_json::Value) -> anyhow::Result<serde_json::Value> {
        ipc_request(&self.socket_path, &payload, DEFAULT_IPC_TIMEOUT)
    }

    pub fn socket_path(&self) -> &Path {
        &self.socket_path
    }

    pub fn generation_counter(&self) -> Arc<AtomicU64> {
        self.generation.clone()
    }

    // True when nothing is loaded or loading; loadfile flips it to false before
    // its reply, so a source a command just started never reads as idle.
    pub fn is_idle(&self) -> anyhow::Result<bool> {
        let reply = self.send(serde_json::json!({ "command": ["get_property", "idle-active"] }))?;
        reply
            .get("data")
            .and_then(|d| d.as_bool())
            .ok_or_else(|| anyhow::anyhow!("mpv returned no idle-active value: {reply}"))
    }
}

impl MpvController for RealMpv {
    fn start_source(&mut self, source: &str) -> anyhow::Result<()> {
        let options = StartOptions {
            known_duration_seconds: None,
            long_source_threshold_seconds: u64::MAX,
            ytdl_format: lofi_common::ytdl_format_for(lofi_common::DEFAULT_AUDIO_QUALITY),
            seek_seconds: None,
        };
        self.start_source_with_options(source, &options)
    }

    fn start_source_with_options(&mut self, source: &str, options: &StartOptions) -> anyhow::Result<()> {
        let generation = self.generation.fetch_add(1, Ordering::SeqCst) + 1;
        // mpv's ytdl hook only falls back to an audio-only selector on its own
        // when ytdl-format is empty; a value from the user's mpv.conf (e.g.
        // bestvideo+bestaudio) would stream video that --no-video throws away.
        // The hook reads it at load time, so it must be set before loadfile.
        self.send(serde_json::json!({ "command": ["set_property", "ytdl-format", options.ytdl_format] }))?;
        // The start option is applied by mpv itself as the file opens, so a
        // chapter position can't race the load the way a later time-pos set
        // would. It persists across loads, hence the explicit reset otherwise.
        let start = options.seek_seconds.map_or_else(|| "none".to_string(), |s| s.to_string());
        self.send(serde_json::json!({ "command": ["set_property", "start", start] }))?;
        self.send(serde_json::json!({ "command": ["loadfile", source, "replace"] }))?;
        // loop-file survives loadfile (and may be set in the user's mpv.conf); a
        // looping source never reaches its end, which would block auto-advance.
        self.send(serde_json::json!({ "command": ["set_property", "loop-file", "no"] }))?;

        if options.seek_seconds.is_some() {
            return Ok(());
        }

        // Duration discovery can take seconds (yt-dlp resolution), and the caller
        // holds the daemon's state lock, so it happens off-thread. The thread only
        // needs the socket path: every IPC request opens its own connection.
        let socket_path = self.socket_path.clone();
        let generation_counter = self.generation.clone();
        let known_duration_seconds = options.known_duration_seconds;
        let long_source_threshold_seconds = options.long_source_threshold_seconds;
        std::thread::spawn(move || {
            seek_into_long_source(
                &socket_path,
                &generation_counter,
                generation,
                known_duration_seconds,
                long_source_threshold_seconds,
            );
        });
        Ok(())
    }

    fn stop(&mut self) -> anyhow::Result<()> {
        self.generation.fetch_add(1, Ordering::SeqCst);
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

    // Callers exit the process right after this, which skips Drop, so mpv must
    // be confirmed gone here. A quit sent while mpv is still starting up can be
    // lost, which was observed to orphan mpv; fall back to killing it.
    fn quit(&mut self) -> anyhow::Result<()> {
        self.generation.fetch_add(1, Ordering::SeqCst);
        let quit_result = ipc_request(
            &self.socket_path,
            &serde_json::json!({ "command": ["quit"] }),
            Duration::from_millis(500),
        );
        let deadline = Instant::now() + Duration::from_secs(1);
        while Instant::now() < deadline {
            if self.child.try_wait()?.is_some() {
                return quit_result.map(|_| ());
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        self.child.kill()?;
        self.child.wait()?;
        Ok(())
    }
}

fn rand_offset_seconds(duration_seconds: u64) -> u64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    let nanos = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().subsec_nanos() as u64;
    let usable_range = duration_seconds.saturating_sub(60).max(1);
    nanos % usable_range
}

impl Drop for RealMpv {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // Stands in for mpv's IPC socket: records every command except property
    // reads, answers get_property duration only once `duration_delay` has
    // passed since the latest loadfile (like real mpv resolving a network
    // source), and answers other reads from `properties`.
    struct FakeMpvServer {
        commands: Arc<std::sync::Mutex<Vec<serde_json::Value>>>,
        _dir: tempfile::TempDir,
        socket_path: PathBuf,
    }

    impl FakeMpvServer {
        fn start(duration_seconds: f64, duration_delay: Duration) -> Self {
            Self::start_with_properties(duration_seconds, duration_delay, serde_json::json!({}))
        }

        fn start_with_properties(duration_seconds: f64, duration_delay: Duration, properties: serde_json::Value) -> Self {
            let dir = tempfile::tempdir().unwrap();
            let socket_path = dir.path().join("fake-mpv.sock");
            let listener = std::os::unix::net::UnixListener::bind(&socket_path).unwrap();
            let commands: Arc<std::sync::Mutex<Vec<serde_json::Value>>> = Arc::default();
            let last_load = Arc::new(std::sync::Mutex::new(Instant::now()));
            let server_commands = commands.clone();
            std::thread::spawn(move || {
                for stream in listener.incoming() {
                    let Ok(stream) = stream else { return };
                    let commands = server_commands.clone();
                    let last_load = last_load.clone();
                    let properties = properties.clone();
                    std::thread::spawn(move || {
                        let mut writer = stream.try_clone().unwrap();
                        // Real mpv pushes events to every client; make sure they are skipped.
                        let _ = writer.write_all(b"{\"event\":\"playback-restart\"}\n");
                        for line in BufReader::new(stream).lines() {
                            let Ok(line) = line else { return };
                            let cmd: serde_json::Value = serde_json::from_str(&line).unwrap();
                            let args = cmd["command"].as_array().unwrap().clone();
                            let reply = if args[0] == "get_property" && args[1] == "duration" {
                                if last_load.lock().unwrap().elapsed() >= duration_delay {
                                    serde_json::json!({ "data": duration_seconds, "error": "success" })
                                } else {
                                    serde_json::json!({ "error": "property unavailable" })
                                }
                            } else if args[0] == "get_property" {
                                match properties.get(args[1].as_str().unwrap()) {
                                    Some(value) => serde_json::json!({ "data": value, "error": "success" }),
                                    None => serde_json::json!({ "error": "property unavailable" }),
                                }
                            } else {
                                if args[0] == "loadfile" {
                                    *last_load.lock().unwrap() = Instant::now();
                                }
                                commands.lock().unwrap().push(cmd.clone());
                                serde_json::json!({ "error": "success" })
                            };
                            let _ = writer.write_all(format!("{reply}\n").as_bytes());
                        }
                    });
                }
            });
            Self { commands, _dir: dir, socket_path }
        }

        fn mpv(&self) -> RealMpv {
            let child = Command::new("sleep").arg("60").spawn().unwrap();
            RealMpv { socket_path: self.socket_path.clone(), child, generation: Arc::default() }
        }

        fn recorded(&self) -> Vec<serde_json::Value> {
            self.commands.lock().unwrap().clone()
        }

        fn wait_for(&self, timeout: Duration, pred: impl Fn(&[serde_json::Value]) -> bool) -> bool {
            let deadline = Instant::now() + timeout;
            while Instant::now() < deadline {
                if pred(&self.recorded()) {
                    return true;
                }
                std::thread::sleep(Duration::from_millis(20));
            }
            false
        }
    }

    fn opts(known_duration_seconds: Option<u64>) -> StartOptions<'static> {
        StartOptions {
            known_duration_seconds,
            long_source_threshold_seconds: 1200,
            ytdl_format: "worstaudio/worst",
            seek_seconds: None,
        }
    }

    // For each loadfile, the start option value set since the previous loadfile.
    fn start_set_before_each_load(recorded: &[serde_json::Value]) -> Vec<Option<String>> {
        let mut result = Vec::new();
        let mut pending = None;
        for cmd in recorded {
            if is_set(cmd, "start") {
                pending = cmd["command"][2].as_str().map(str::to_string);
            } else if cmd["command"][0] == "loadfile" {
                result.push(pending.take());
            }
        }
        result
    }

    #[test]
    fn explicit_chapter_seek_starts_the_file_there_and_skips_the_random_seek() {
        let server = FakeMpvServer::start(3.0 * 3600.0, Duration::ZERO);
        let mut mpv = server.mpv();
        let options = StartOptions { seek_seconds: Some(754), ..opts(Some(3 * 3600)) };
        mpv.start_source_with_options("https://example.com/3h-mix", &options).unwrap();
        std::thread::sleep(Duration::from_millis(800));
        let recorded = server.recorded();
        assert_eq!(start_set_before_each_load(&recorded), vec![Some("754".to_string())]);
        assert!(
            !recorded.iter().any(|c| is_set(c, "time-pos")),
            "a random seek would override the chapter the user picked: {recorded:?}"
        );
    }

    #[test]
    fn every_start_without_a_seek_resets_the_start_option() {
        let server = FakeMpvServer::start(180.0, Duration::ZERO);
        let mut mpv = server.mpv();
        mpv.start_source_with_options("https://example.com/mix", &StartOptions { seek_seconds: Some(60), ..opts(None) }).unwrap();
        mpv.start_source_with_options("/music/local.flac", &opts(None)).unwrap();
        assert_eq!(
            start_set_before_each_load(&server.recorded()),
            vec![Some("60".to_string()), Some("none".to_string())],
            "a chapter offset must never leak into the next source"
        );
    }

    #[test]
    fn chapter_seek_makes_a_pending_probe_for_the_previous_source_stale() {
        let server = FakeMpvServer::start(3.0 * 3600.0, Duration::from_millis(600));
        let mut mpv = server.mpv();
        mpv.start_source_with_options("https://example.com/3h-mix", &opts(None)).unwrap();
        mpv.start_source_with_options("https://example.com/3h-mix", &StartOptions { seek_seconds: Some(754), ..opts(None) })
            .unwrap();
        std::thread::sleep(Duration::from_millis(1500));
        assert!(
            !server.recorded().iter().any(|c| is_set(c, "time-pos")),
            "the earlier load's probe overrode the chapter seek: {:?}",
            server.recorded()
        );
    }

    fn is_set(cmd: &serde_json::Value, property: &str) -> bool {
        cmd["command"][0] == "set_property" && cmd["command"][1] == property
    }

    #[test]
    fn ytdl_format_is_set_before_every_loadfile_so_video_is_never_fetched() {
        let server = FakeMpvServer::start(180.0, Duration::ZERO);
        let mut mpv = server.mpv();
        let options = StartOptions { ytdl_format: "bestaudio/worst", ..opts(None) };
        mpv.start_source_with_options("https://example.com/watch?v=abc", &options).unwrap();
        mpv.start_source_with_options("/music/local.flac", &opts(None)).unwrap();

        assert_eq!(
            format_set_before_each_load(&server.recorded()),
            vec![Some("bestaudio/worst".to_string()), Some("worstaudio/worst".to_string())]
        );
    }

    // For each loadfile, the ytdl-format value set since the previous loadfile.
    fn format_set_before_each_load(recorded: &[serde_json::Value]) -> Vec<Option<String>> {
        let mut result = Vec::new();
        let mut pending = None;
        for cmd in recorded {
            if is_set(cmd, "ytdl-format") {
                pending = cmd["command"][2].as_str().map(str::to_string);
            } else if cmd["command"][0] == "loadfile" {
                result.push(pending.take());
            }
        }
        result
    }

    #[test]
    fn every_source_start_resets_loop_file_right_after_loadfile() {
        let server = FakeMpvServer::start(180.0, Duration::ZERO);
        let mut mpv = server.mpv();
        mpv.start_source_with_options("short.mp3", &opts(None)).unwrap();

        let recorded = server.recorded();
        let load = recorded.iter().position(|c| c["command"][0] == "loadfile").expect("no loadfile");
        assert_eq!(recorded[load]["command"], serde_json::json!(["loadfile", "short.mp3", "replace"]));
        assert_eq!(recorded[load + 1]["command"], serde_json::json!(["set_property", "loop-file", "no"]));
    }

    #[test]
    fn short_source_after_a_long_one_is_not_seeked() {
        let server = FakeMpvServer::start(180.0, Duration::ZERO);
        let mut mpv = server.mpv();
        mpv.start_source_with_options("long.mp4", &opts(Some(3 * 3600))).unwrap();
        assert!(server.wait_for(Duration::from_secs(3), |c| c.iter().any(|c| is_set(c, "time-pos"))));

        mpv.start_source_with_options("short.mp3", &opts(None)).unwrap();
        std::thread::sleep(Duration::from_millis(800));
        let recorded = server.recorded();
        let after_short: Vec<_> = recorded
            .iter()
            .skip_while(|c| c["command"][1] != "short.mp3")
            .collect();
        assert_eq!(after_short[1]["command"], serde_json::json!(["set_property", "loop-file", "no"]));
        assert!(
            !after_short.iter().any(|c| is_set(c, "time-pos") || c["command"][2] == "inf"),
            "a 3 minute source must not be seeked or looped: {after_short:?}"
        );
    }

    #[test]
    fn long_source_start_returns_before_duration_is_known_then_seeks_in_background() {
        let server = FakeMpvServer::start(3.0 * 3600.0, Duration::from_millis(1500));
        let mut mpv = server.mpv();
        let started = Instant::now();
        mpv.start_source_with_options("https://example.com/3h-mix", &opts(None)).unwrap();
        assert!(started.elapsed() < Duration::from_millis(500), "start blocked on duration discovery");
        assert!(!server.recorded().iter().any(|c| is_set(c, "time-pos")));

        assert!(server.wait_for(Duration::from_secs(5), |c| c.iter().any(|c| is_set(c, "time-pos"))));
        let recorded = server.recorded();
        let seek = recorded.iter().position(|c| is_set(c, "time-pos")).expect("no seek");
        let reset = recorded.iter().position(|c| c["command"][2] == "no").unwrap();
        assert!(reset < seek, "wrong order: {recorded:?}");
    }

    #[test]
    fn long_source_is_never_set_to_loop_forever_so_it_can_reach_its_end() {
        let server = FakeMpvServer::start(3.0 * 3600.0, Duration::ZERO);
        let mut mpv = server.mpv();
        mpv.start_source_with_options("https://example.com/3h-mix", &opts(None)).unwrap();
        assert!(server.wait_for(Duration::from_secs(3), |c| c.iter().any(|c| is_set(c, "time-pos"))));
        std::thread::sleep(Duration::from_millis(300));
        assert!(
            !server.recorded().iter().any(|c| is_set(c, "loop-file") && c["command"][2] != "no"),
            "loop-file inf would stop the source from ever ending and auto-advancing"
        );
    }

    #[test]
    fn stale_background_probe_does_not_seek_a_newer_source() {
        let server = FakeMpvServer::start(3.0 * 3600.0, Duration::from_millis(600));
        let mut mpv = server.mpv();
        mpv.start_source_with_options("https://example.com/3h-mix", &opts(None)).unwrap();
        mpv.start_source_with_options("short.mp3", &opts(Some(180))).unwrap();
        std::thread::sleep(Duration::from_millis(1500));
        let recorded = server.recorded();
        assert!(
            !recorded.iter().any(|c| is_set(c, "time-pos") || c["command"][2] == "inf"),
            "probe for the replaced source acted on the new one: {recorded:?}"
        );
    }

    #[test]
    fn generation_bump_after_duration_probe_prevents_stale_seek() {
        let server = FakeMpvServer::start(3.0 * 3600.0, Duration::from_millis(800));
        let generation = Arc::new(AtomicU64::new(1));
        let gen_clone = generation.clone();
        let socket_path = server.socket_path.clone();

        let probe_thread = std::thread::spawn(move || {
            seek_into_long_source(&socket_path, &gen_clone, 1, None, 1200);
        });

        std::thread::sleep(Duration::from_millis(400));
        generation.store(2, Ordering::SeqCst);

        probe_thread.join().unwrap();
        std::thread::sleep(Duration::from_millis(200));

        assert!(
            !server.recorded().iter().any(|c| is_set(c, "time-pos")),
            "generation bump during probe should prevent the seek: {:?}",
            server.recorded()
        );
    }

    // Real mpv on a YouTube live stream reports the time since the broadcast
    // started as an ever-growing duration (72054s was observed) and seekable=true.
    #[test]
    fn live_stream_with_a_growing_duration_is_never_randomly_seeked() {
        let server = FakeMpvServer::start_with_properties(
            72054.9,
            Duration::ZERO,
            serde_json::json!({ "metadata/by-key/ytdl_is_live": "true", "seekable": true }),
        );
        let mut mpv = server.mpv();
        mpv.start_source_with_options("https://www.youtube.com/watch?v=live", &opts(None)).unwrap();
        std::thread::sleep(Duration::from_millis(1200));
        assert!(!server.recorded().iter().any(|c| is_set(c, "time-pos")), "{:?}", server.recorded());
    }

    #[test]
    fn unseekable_long_source_is_not_randomly_seeked() {
        let server =
            FakeMpvServer::start_with_properties(3.0 * 3600.0, Duration::ZERO, serde_json::json!({ "seekable": false }));
        let mut mpv = server.mpv();
        mpv.start_source_with_options("https://example.com/radio", &opts(None)).unwrap();
        std::thread::sleep(Duration::from_millis(1200));
        assert!(!server.recorded().iter().any(|c| is_set(c, "time-pos")));
    }

    #[test]
    fn duration_probe_gives_up_within_its_budget_when_no_duration_ever_appears() {
        let server = FakeMpvServer::start(3.0 * 3600.0, Duration::from_secs(3600));
        let generation = AtomicU64::new(1);
        let started = Instant::now();
        assert_eq!(query_duration(&server.socket_path, &generation, 1, Duration::from_secs(1)), None);
        assert!(started.elapsed() < Duration::from_secs(3), "probe overran its budget: {:?}", started.elapsed());
    }

    #[test]
    fn a_zero_duration_is_not_mistaken_for_a_real_one() {
        let server = FakeMpvServer::start(0.0, Duration::ZERO);
        let generation = AtomicU64::new(1);
        assert_eq!(query_duration(&server.socket_path, &generation, 1, Duration::from_secs(1)), None);
    }

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
    fn spawn_replaces_a_stale_socket_file_and_waits_for_a_live_one() {
        if !mpv_available() {
            eprintln!("skipping: mpv not installed in this environment");
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        let socket_path = dir.path().join("mpv-stale.sock");
        // A bound-then-dropped listener leaves a socket file nobody accepts on.
        drop(std::os::unix::net::UnixListener::bind(&socket_path).unwrap());
        assert!(UnixStream::connect(&socket_path).is_err());

        let mut mpv = RealMpv::spawn(socket_path.clone()).unwrap();
        assert!(UnixStream::connect(&socket_path).is_ok());
        let mode = std::fs::metadata(&socket_path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600, "mpv IPC socket must be owner-only");
        mpv.quit().unwrap();
        assert!(mpv.child.try_wait().unwrap().is_some(), "quit must leave mpv exited");
    }

    #[test]
    fn find_mpris_script_prefers_env_override_when_file_exists() {
        let _guard = crate::ENV_MUTEX.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        let dir = tempfile::tempdir().unwrap();
        let fake_script = dir.path().join("mpris.so");
        std::fs::write(&fake_script, b"").unwrap();
        std::env::set_var("LOFI_MPV_MPRIS_SCRIPT", fake_script.to_str().unwrap());
        assert_eq!(find_mpris_script(), Some(fake_script.to_str().unwrap().to_string()));
        std::env::remove_var("LOFI_MPV_MPRIS_SCRIPT");
    }

    #[test]
    fn find_mpris_script_ignores_env_override_pointing_at_missing_file() {
        let _guard = crate::ENV_MUTEX.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        std::env::set_var("LOFI_MPV_MPRIS_SCRIPT", "/nonexistent/mpris.so");
        let result = find_mpris_script();
        std::env::remove_var("LOFI_MPV_MPRIS_SCRIPT");
        assert_ne!(result, Some("/nonexistent/mpris.so".to_string()));
    }

    #[test]
    fn static_candidates_include_real_world_distro_paths() {
        assert!(MPRIS_SCRIPT_CANDIDATES.contains(&"/usr/lib/mpv-mpris/mpris.so"));
        assert!(MPRIS_SCRIPT_CANDIDATES.contains(&"/etc/mpv/scripts/mpris.so"));
    }

    #[test]
    fn spawn_includes_keep_open_and_loop_playlist_overrides_to_guarantee_auto_advance() {
        let mut cmd = Command::new(mpv_binary());
        cmd.arg("--idle")
            .arg("--no-video")
            .arg("--keep-open=no")
            .arg("--loop-playlist=no")
            .arg("--input-ipc-server=/tmp/test.sock");
        let args: Vec<String> = cmd.get_args().map(|s| s.to_string_lossy().to_string()).collect();
        assert!(
            args.contains(&"--keep-open=no".to_string()),
            "spawn must include --keep-open=no to override user mpv.conf, got args: {args:?}"
        );
        assert!(
            args.contains(&"--loop-playlist=no".to_string()),
            "spawn must include --loop-playlist=no to override user mpv.conf, got args: {args:?}"
        );
    }

    #[test]
    fn find_mpris_script_falls_back_to_user_config_dir() {
        let _guard = crate::ENV_MUTEX.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        let dir = tempfile::tempdir().unwrap();
        let scripts_dir = dir.path().join("mpv").join("scripts");
        std::fs::create_dir_all(&scripts_dir).unwrap();
        let fake_script = scripts_dir.join("mpris.so");
        std::fs::write(&fake_script, b"").unwrap();

        std::env::set_var("XDG_CONFIG_HOME", dir.path());
        let result = find_mpris_script();
        std::env::remove_var("XDG_CONFIG_HOME");

        // Only asserts the dynamic path is picked up when none of the static
        // candidates exist on the machine running this test.
        if !MPRIS_SCRIPT_CANDIDATES.iter().any(|p| std::path::Path::new(p).exists()) {
            assert_eq!(result, Some(fake_script.to_str().unwrap().to_string()));
        }
    }
}
