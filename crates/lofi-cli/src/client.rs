use std::io::{BufRead, BufReader, Read, Write};
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

fn daemon_binary() -> std::path::PathBuf {
    if let Ok(path) = std::env::var("LOFI_DAEMON_BIN") {
        return std::path::PathBuf::from(path);
    }
    match std::env::current_exe() {
        Ok(exe) => match exe.parent() {
            Some(dir) => dir.join("lofi-daemon"),
            None => std::path::PathBuf::from("lofi-daemon"),
        },
        Err(_) => std::path::PathBuf::from("lofi-daemon"),
    }
}

// Bounded so a misbehaving daemon binary can't make the CLI buffer unbounded output.
const MAX_DAEMON_STDERR_BYTES: u64 = 64 * 1024;

pub fn ensure_daemon_running(socket_path: &Path) -> anyhow::Result<()> {
    if UnixStream::connect(socket_path).is_ok() {
        return Ok(());
    }
    // A stale socket file is left for the daemon to replace: it only does so
    // after taking its single-instance lock, whereas deleting it here could
    // unlink the socket of a daemon another terminal just started.
    let mut child = std::process::Command::new(daemon_binary())
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::piped())
        .spawn()?;

    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    let mut child_exited_cleanly = false;
    while UnixStream::connect(socket_path).is_err() {
        if !child_exited_cleanly {
            if let Some(status) = child.try_wait()? {
                // Exit 0 means another daemon already holds the instance lock
                // (several terminals opened at once); keep waiting for its socket.
                if status.success() {
                    child_exited_cleanly = true;
                } else {
                    let mut message = String::new();
                    if let Some(stderr) = child.stderr.take() {
                        let _ = stderr.take(MAX_DAEMON_STDERR_BYTES).read_to_string(&mut message);
                    }
                    let message = message.trim();
                    if message.is_empty() {
                        anyhow::bail!("lofi-daemon failed to start ({status})");
                    }
                    anyhow::bail!("lofi-daemon failed to start: {message}");
                }
            }
        }
        if std::time::Instant::now() > deadline {
            anyhow::bail!("lofi-daemon did not start within 5 seconds");
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    Ok(())
}

// Single-video metadata (with its full formats list) is well under this.
const MAX_YT_DLP_OUTPUT_BYTES: u64 = 16 * 1024 * 1024;

// mpv runs inside the daemon, whose working directory is whichever shell
// happened to spawn it, so a relative path must be made absolute here.
pub fn resolve_source(source: &str) -> anyhow::Result<String> {
    let path = Path::new(source);
    if !path.exists() {
        return Ok(source.to_string());
    }
    let canonical = std::fs::canonicalize(path)
        .map_err(|e| anyhow::anyhow!("could not resolve local path '{source}': {e}"))?;
    canonical
        .into_os_string()
        .into_string()
        .map_err(|_| anyhow::anyhow!("local path '{source}' is not valid UTF-8"))
}

// Anything that is not an existing file and has no URL scheme is also treated
// as local (e.g. a file deleted since it was added): handing it to yt-dlp would
// never find anything, and local sources must never cost a network call.
pub fn is_local_source(source: &str) -> bool {
    Path::new(source).exists() || !source.contains("://")
}

// Errors are returned as messages rather than printed, since the TUI calls
// this while it owns the terminal and stray stderr output would corrupt it.
fn fetch_yt_dlp_metadata(source: &str) -> Result<serde_json::Value, String> {
    let not_detected = || format!("yt-dlp not detected or failed to fetch metadata for '{source}'");
    let yt_dlp_bin = std::env::var("LOFI_YTDLP_BIN").unwrap_or_else(|_| "yt-dlp".to_string());
    let child = std::process::Command::new(&yt_dlp_bin)
        .arg("--dump-json")
        .arg("--skip-download")
        // A source is one playable unit; a playlist URL would otherwise emit
        // one JSON document per entry, unbounded and unparseable as one value.
        .arg("--no-playlist")
        .arg("--socket-timeout")
        .arg("10")
        // Without the separator a source like "--config-locations=..." is
        // parsed by yt-dlp as an option instead of a URL.
        .arg("--")
        .arg(source)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .spawn();
    let mut child = child.map_err(|_| not_detected())?;
    let mut stdout = Vec::new();
    if let Some(out) = child.stdout.take() {
        let _ = out.take(MAX_YT_DLP_OUTPUT_BYTES + 1).read_to_end(&mut stdout);
    }
    if stdout.len() as u64 > MAX_YT_DLP_OUTPUT_BYTES {
        let _ = child.kill();
        let _ = child.wait();
        return Err(format!("yt-dlp returned unexpectedly large metadata for '{source}', ignoring it"));
    }
    match child.wait() {
        Ok(status) if status.success() => {}
        _ => return Err(not_detected()),
    }
    serde_json::from_slice(&stdout).map_err(|e| format!("could not parse yt-dlp metadata for '{source}': {e}"))
}

pub fn classify_source(source: &str) -> anyhow::Result<Option<String>> {
    let is_local = std::path::Path::new(source).exists();
    if is_local {
        return Ok(None);
    }
    let json = match fetch_yt_dlp_metadata(source) {
        Ok(json) => json,
        Err(message) => {
            eprintln!("{message}");
            return Ok(None);
        }
    };
    let title = json.get("title").and_then(|v| v.as_str()).unwrap_or("");
    let description = json.get("description").and_then(|v| v.as_str()).unwrap_or("");

    // Package installs never seed config.toml, so fall back to the shipped
    // default classifier keywords rather than failing on a missing file.
    let config = lofi_common::load_config_or_default(&lofi_common::config_path())?;
    Ok(lofi_common::classify(&config.classifier, title, description))
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Chapter {
    pub title: String,
    pub start_seconds: u64,
}

// Bounds on what one source's metadata can put on screen.
pub const MAX_CHAPTERS: usize = 1000;
pub const MAX_DISPLAY_CHARS: usize = 200;

// Text from a source's metadata is not ours; control characters (escape
// sequences in particular) would reach the terminal through the TUI.
pub fn sanitize_for_display(text: &str) -> String {
    text.chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .take(MAX_DISPLAY_CHARS)
        .collect::<String>()
        .trim()
        .to_string()
}

pub fn parse_chapters(json: &serde_json::Value) -> Vec<Chapter> {
    let Some(entries) = json.get("chapters").and_then(|c| c.as_array()) else {
        return Vec::new();
    };
    entries
        .iter()
        .enumerate()
        .filter_map(|(i, entry)| {
            let start = entry.get("start_time")?.as_f64()?;
            if !start.is_finite() || start < 0.0 {
                return None;
            }
            let title = entry
                .get("title")
                .and_then(|t| t.as_str())
                .map(sanitize_for_display)
                .filter(|t| !t.is_empty())
                .unwrap_or_else(|| format!("chapter {}", i + 1));
            Some(Chapter { title, start_seconds: start as u64 })
        })
        .take(MAX_CHAPTERS)
        .collect()
}

pub fn fetch_chapters(source: &str) -> anyhow::Result<Vec<Chapter>> {
    if is_local_source(source) {
        return Ok(Vec::new());
    }
    let json = fetch_yt_dlp_metadata(source).map_err(|message| anyhow::anyhow!(message))?;
    Ok(parse_chapters(&json))
}

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
            let mut reader = BufReader::new(stream);
            let mut line = String::new();
            reader.read_line(&mut line).unwrap();
            let resp = lofi_common::encode_response(&lofi_common::Response::Ok);
            writer.write_all(resp.as_bytes()).unwrap();
            let _ = server_socket_path;
        });

        std::thread::sleep(std::time::Duration::from_millis(50));
        let resp = send_command(&socket_path, &lofi_common::Command::Register).unwrap();
        assert!(matches!(resp, lofi_common::Response::Ok));
    }

    fn env_lock() -> std::sync::MutexGuard<'static, ()> {
        crate::ENV_MUTEX.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    fn fake_daemon_script(dir: &std::path::Path, body: &str) -> std::path::PathBuf {
        use std::os::unix::fs::PermissionsExt;
        let path = dir.join("fake-lofi-daemon");
        std::fs::write(&path, format!("#!/bin/sh\n{body}\n")).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        path
    }

    #[test]
    fn ensure_daemon_running_reports_daemon_stderr_immediately_on_failure() {
        let _guard = env_lock();
        let dir = tempfile::tempdir().unwrap();
        let script = fake_daemon_script(dir.path(), "echo 'mpv not detected, install it' >&2\nexit 1");
        std::env::set_var("LOFI_DAEMON_BIN", &script);
        let started = std::time::Instant::now();
        let result = ensure_daemon_running(&dir.path().join("never.sock"));
        std::env::remove_var("LOFI_DAEMON_BIN");

        let err = result.unwrap_err().to_string();
        assert!(err.contains("mpv not detected"), "stderr not surfaced: {err}");
        assert!(started.elapsed() < Duration::from_secs(2), "should not wait out the timeout");
    }

    #[test]
    fn ensure_daemon_running_keeps_waiting_when_spawned_daemon_lost_the_instance_race() {
        let _guard = env_lock();
        let dir = tempfile::tempdir().unwrap();
        let script = fake_daemon_script(dir.path(), "echo 'another lofi-daemon is already running' >&2\nexit 0");
        let socket_path = dir.path().join("winner.sock");
        let winner_path = socket_path.clone();
        let winner = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(400));
            UnixListener::bind(&winner_path).unwrap()
        });
        std::env::set_var("LOFI_DAEMON_BIN", &script);
        let result = ensure_daemon_running(&socket_path);
        std::env::remove_var("LOFI_DAEMON_BIN");
        drop(winner.join().unwrap());
        result.unwrap();
    }

    fn fake_yt_dlp_script(dir: &std::path::Path, stdout: &str) -> std::path::PathBuf {
        use std::os::unix::fs::PermissionsExt;
        let path = dir.join("fake-yt-dlp");
        let out_file = dir.join("yt-dlp-stdout");
        std::fs::write(&out_file, stdout).unwrap();
        let args_file = dir.join("yt-dlp-args");
        std::fs::write(
            &path,
            format!(
                "#!/bin/sh\nfor a; do printf '%s\\n' \"$a\"; done > '{}'\ncat '{}'\n",
                args_file.display(),
                out_file.display()
            ),
        )
        .unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        path
    }

    #[test]
    fn classify_source_uses_default_classifier_when_no_config_file_exists() {
        let _guard = env_lock();
        let dir = tempfile::tempdir().unwrap();
        let yt_dlp = fake_yt_dlp_script(
            dir.path(),
            r#"{"title": "Heavy rain and thunder for sleeping", "description": ""}"#,
        );
        std::env::set_var("LOFI_YTDLP_BIN", &yt_dlp);
        std::env::set_var("XDG_CONFIG_HOME", dir.path().join("empty-config-home"));
        let result = classify_source("https://example.com/watch?v=abc");
        std::env::remove_var("LOFI_YTDLP_BIN");
        std::env::remove_var("XDG_CONFIG_HOME");
        assert_eq!(result.unwrap(), Some("rainy-day".to_string()));
    }

    #[test]
    fn classify_source_handles_live_stream_metadata_without_a_duration() {
        let _guard = env_lock();
        let dir = tempfile::tempdir().unwrap();
        let yt_dlp = fake_yt_dlp_script(
            dir.path(),
            r#"{"title": "lofi rain radio 24/7", "description": "", "is_live": true, "duration": null}"#,
        );
        std::env::set_var("LOFI_YTDLP_BIN", &yt_dlp);
        std::env::set_var("XDG_CONFIG_HOME", dir.path().join("empty-config-home"));
        let result = classify_source("https://www.youtube.com/watch?v=live");
        std::env::remove_var("LOFI_YTDLP_BIN");
        std::env::remove_var("XDG_CONFIG_HOME");
        assert_eq!(result.unwrap(), Some("rainy-day".to_string()));
    }

    #[test]
    fn resolve_source_makes_existing_local_paths_absolute_and_canonical() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir(dir.path().join("sub")).unwrap();
        let file = dir.path().join("mix.mp3");
        std::fs::write(&file, b"").unwrap();
        let indirect = dir.path().join("sub").join("..").join("mix.mp3");

        let resolved = resolve_source(indirect.to_str().unwrap()).unwrap();
        assert_eq!(resolved, std::fs::canonicalize(&file).unwrap().to_str().unwrap());
        assert!(std::path::Path::new(&resolved).is_absolute());
        assert!(!resolved.contains(".."));
    }

    #[test]
    fn resolve_source_leaves_urls_and_missing_paths_untouched() {
        assert_eq!(
            resolve_source("https://www.youtube.com/watch?v=abc").unwrap(),
            "https://www.youtube.com/watch?v=abc"
        );
        assert_eq!(resolve_source("./definitely-missing.mp3").unwrap(), "./definitely-missing.mp3");
    }

    fn recorded_yt_dlp_args(dir: &std::path::Path) -> Vec<String> {
        std::fs::read_to_string(dir.join("yt-dlp-args"))
            .unwrap()
            .lines()
            .map(str::to_string)
            .collect()
    }

    #[test]
    fn classify_source_passes_option_like_sources_after_a_separator() {
        let _guard = env_lock();
        let dir = tempfile::tempdir().unwrap();
        let yt_dlp = fake_yt_dlp_script(dir.path(), r#"{"title": "x", "description": ""}"#);
        std::env::set_var("LOFI_YTDLP_BIN", &yt_dlp);
        let _ = classify_source("--config-locations=/tmp/evil.conf");
        std::env::remove_var("LOFI_YTDLP_BIN");

        let args = recorded_yt_dlp_args(dir.path());
        let n = args.len();
        assert_eq!(args[n - 2], "--", "missing -- separator: {args:?}");
        assert_eq!(args[n - 1], "--config-locations=/tmp/evil.conf");
    }

    #[test]
    fn classify_source_limits_yt_dlp_to_one_video_with_a_network_timeout() {
        let _guard = env_lock();
        let dir = tempfile::tempdir().unwrap();
        let yt_dlp = fake_yt_dlp_script(dir.path(), r#"{"title": "x", "description": ""}"#);
        std::env::set_var("LOFI_YTDLP_BIN", &yt_dlp);
        let _ = classify_source("https://www.youtube.com/watch?v=abc&list=PLxyz");
        std::env::remove_var("LOFI_YTDLP_BIN");

        let args = recorded_yt_dlp_args(dir.path());
        let separator = args.iter().position(|a| a == "--").unwrap();
        let options = &args[..separator];
        assert!(options.contains(&"--no-playlist".to_string()), "{args:?}");
        let timeout = options.iter().position(|a| a == "--socket-timeout").expect("no --socket-timeout");
        assert_eq!(options[timeout + 1], "10");
    }

    #[test]
    fn classify_source_degrades_gracefully_on_multi_document_output() {
        let _guard = env_lock();
        let dir = tempfile::tempdir().unwrap();
        let yt_dlp = fake_yt_dlp_script(
            dir.path(),
            "{\"title\": \"rain one\"}\n{\"title\": \"rain two\"}\n",
        );
        std::env::set_var("LOFI_YTDLP_BIN", &yt_dlp);
        let result = classify_source("https://www.youtube.com/playlist?list=PLxyz");
        std::env::remove_var("LOFI_YTDLP_BIN");
        assert_eq!(result.unwrap(), None);
    }

    const SAMPLE_CHAPTERS_JSON: &str = r#"{
        "title": "lofi hip hop mix",
        "is_live": false,
        "duration": 5020.0,
        "chapters": [
            {"start_time": 0.0, "end_time": 151.0, "title": "Kupla - Owls of the Night"},
            {"start_time": 151.0, "end_time": 312.5, "title": "j'san. x nymano - autumn breeze"},
            {"start_time": 312.5, "end_time": 5020.0, "title": "Mondo Loops - Late Night Feelings"}
        ]
    }"#;

    #[test]
    fn parse_chapters_reads_titles_and_whole_second_start_times() {
        let json: serde_json::Value = serde_json::from_str(SAMPLE_CHAPTERS_JSON).unwrap();
        assert_eq!(
            parse_chapters(&json),
            vec![
                Chapter { title: "Kupla - Owls of the Night".to_string(), start_seconds: 0 },
                Chapter { title: "j'san. x nymano - autumn breeze".to_string(), start_seconds: 151 },
                Chapter { title: "Mondo Loops - Late Night Feelings".to_string(), start_seconds: 312 },
            ]
        );
    }

    #[test]
    fn parse_chapters_degrades_gracefully_on_missing_or_malformed_data() {
        for json in [r#"{"title": "x"}"#, r#"{"chapters": null}"#, r#"{"chapters": "nope"}"#, r#"{"chapters": []}"#] {
            assert!(parse_chapters(&serde_json::from_str(json).unwrap()).is_empty(), "{json}");
        }
        let json: serde_json::Value = serde_json::from_str(
            r#"{"chapters": [
                {"title": "no start"},
                {"start_time": -5, "title": "negative"},
                {"start_time": "12", "title": "string start"},
                {"start_time": 42.9},
                {"start_time": 60, "title": "evil\u001b[2Jtitle\nnext"}
            ]}"#,
        )
        .unwrap();
        assert_eq!(
            parse_chapters(&json),
            vec![
                Chapter { title: "chapter 4".to_string(), start_seconds: 42 },
                Chapter { title: "evil [2Jtitle next".to_string(), start_seconds: 60 },
            ]
        );
    }

    #[test]
    fn parse_chapters_clips_overlong_titles_and_caps_the_count() {
        let long_title = "x".repeat(10_000);
        let entries: Vec<String> =
            (0..5_000).map(|i| format!(r#"{{"start_time": {i}, "title": "{long_title}"}}"#)).collect();
        let json: serde_json::Value = serde_json::from_str(&format!(r#"{{"chapters": [{}]}}"#, entries.join(","))).unwrap();
        let chapters = parse_chapters(&json);
        assert_eq!(chapters.len(), MAX_CHAPTERS);
        assert!(chapters.iter().all(|c| c.title.chars().count() <= MAX_DISPLAY_CHARS));
    }

    #[test]
    fn fetch_chapters_never_runs_yt_dlp_for_a_local_source() {
        let _guard = env_lock();
        let dir = tempfile::tempdir().unwrap();
        let yt_dlp = fake_yt_dlp_script(dir.path(), SAMPLE_CHAPTERS_JSON);
        let local = dir.path().join("mix.flac");
        std::fs::write(&local, b"").unwrap();
        std::env::set_var("LOFI_YTDLP_BIN", &yt_dlp);
        let existing = fetch_chapters(local.to_str().unwrap());
        let missing = fetch_chapters("/music/deleted-since.flac");
        std::env::remove_var("LOFI_YTDLP_BIN");
        assert!(existing.unwrap().is_empty());
        assert!(missing.unwrap().is_empty());
        assert!(!dir.path().join("yt-dlp-args").exists(), "yt-dlp was invoked for a local source");
    }

    #[test]
    fn fetch_chapters_for_a_url_uses_the_same_safe_yt_dlp_invocation() {
        let _guard = env_lock();
        let dir = tempfile::tempdir().unwrap();
        let yt_dlp = fake_yt_dlp_script(dir.path(), SAMPLE_CHAPTERS_JSON);
        std::env::set_var("LOFI_YTDLP_BIN", &yt_dlp);
        let chapters = fetch_chapters("https://www.youtube.com/watch?v=abc&list=PLxyz");
        std::env::remove_var("LOFI_YTDLP_BIN");
        assert_eq!(chapters.unwrap().len(), 3);
        let args = recorded_yt_dlp_args(dir.path());
        let separator = args.iter().position(|a| a == "--").expect("missing -- separator");
        assert_eq!(args[separator + 1], "https://www.youtube.com/watch?v=abc&list=PLxyz");
        assert!(args[..separator].contains(&"--no-playlist".to_string()));
        assert!(args[..separator].contains(&"--socket-timeout".to_string()));
    }

    #[test]
    fn fetch_chapters_reports_a_missing_yt_dlp_as_an_error() {
        let _guard = env_lock();
        std::env::set_var("LOFI_YTDLP_BIN", "/nonexistent/definitely-not-yt-dlp");
        let result = fetch_chapters("https://example.com/some-video");
        std::env::remove_var("LOFI_YTDLP_BIN");
        assert!(result.is_err());
    }

    #[test]
    fn is_local_source_treats_anything_without_a_url_scheme_as_local() {
        assert!(is_local_source("/music/a.flac"));
        assert!(is_local_source("/music/deleted-since.flac"));
        assert!(!is_local_source("https://www.youtube.com/watch?v=abc"));
        assert!(!is_local_source("ytdl://abc"));
    }

    #[test]
    fn daemon_binary_respects_env_override() {
        let _guard = env_lock();
        std::env::set_var("LOFI_DAEMON_BIN", "/tmp/some-custom-lofi-daemon");
        let resolved = daemon_binary();
        std::env::remove_var("LOFI_DAEMON_BIN");
        assert_eq!(resolved, std::path::PathBuf::from("/tmp/some-custom-lofi-daemon"));
    }

    #[test]
    fn classify_source_degrades_gracefully_when_yt_dlp_is_missing() {
        let _guard = env_lock();
        std::env::set_var("LOFI_YTDLP_BIN", "/nonexistent/definitely-not-yt-dlp");
        let result = classify_source("https://example.com/some-video");
        std::env::remove_var("LOFI_YTDLP_BIN");
        assert_eq!(result.unwrap(), None);
    }
}
