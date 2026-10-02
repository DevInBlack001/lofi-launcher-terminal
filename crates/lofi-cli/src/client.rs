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

fn yt_dlp_binary() -> String {
    std::env::var("LOFI_YTDLP_BIN").unwrap_or_else(|_| "yt-dlp".to_string())
}

// Errors are returned as messages rather than printed, since the TUI calls
// this while it owns the terminal and stray stderr output would corrupt it.
fn fetch_yt_dlp_metadata(source: &str) -> Result<serde_json::Value, String> {
    // A source is one playable unit; a playlist URL would otherwise emit one
    // JSON document per entry, unbounded and unparseable as one value.
    run_yt_dlp_json(&["--dump-json", "--skip-download", "--no-playlist"], source)
}

fn run_yt_dlp_json(options: &[&str], source: &str) -> Result<serde_json::Value, String> {
    let not_detected = || format!("yt-dlp not detected or failed to fetch metadata for '{source}'");
    let child = std::process::Command::new(yt_dlp_binary())
        .args(options)
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
pub struct VideoInfo {
    pub title: String,
    pub description: String,
    pub is_live: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlaylistEntry {
    // 1-based position in the playlist as yt-dlp listed it.
    pub position: usize,
    pub url: Option<String>,
    // None for private or deleted videos, which yt-dlp still lists.
    pub title: Option<String>,
    pub description: String,
    pub is_live: bool,
    pub nested_playlist: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Probe {
    Single(VideoInfo),
    Playlist {
        title: String,
        entries: Vec<PlaylistEntry>,
        // Entry count before capping at MAX_PLAYLIST_ENTRIES, when capped.
        truncated_from: Option<usize>,
    },
    // The probe failed or returned a shape we do not recognize; callers fall
    // back to treating the URL as a single video.
    Unknown,
}

// Every entry becomes a config.toml line and a daemon round trip.
pub const MAX_PLAYLIST_ENTRIES: usize = 1000;

fn is_youtube_video_id(id: &str) -> bool {
    id.len() == 11 && id.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
}

fn entry_url(entry: &serde_json::Value) -> Option<String> {
    if let Some(url) = entry.get("url").and_then(|u| u.as_str()) {
        if url.starts_with("https://") || url.starts_with("http://") {
            return Some(url.to_string());
        }
    }
    // Only YouTube ids map onto a URL we can build; other extractors' relative
    // "url" values have no known base.
    let ie_key = entry.get("ie_key").and_then(|k| k.as_str());
    let id = entry.get("id").and_then(|i| i.as_str())?;
    if matches!(ie_key, None | Some("Youtube")) && is_youtube_video_id(id) {
        return Some(format!("https://www.youtube.com/watch?v={id}"));
    }
    None
}

fn is_live_json(json: &serde_json::Value) -> bool {
    json.get("is_live").and_then(|v| v.as_bool()).unwrap_or(false)
        || json.get("live_status").and_then(|v| v.as_str()) == Some("is_live")
}

fn parse_playlist_entry(position: usize, entry: &serde_json::Value) -> PlaylistEntry {
    let title = entry
        .get("title")
        .and_then(|t| t.as_str())
        .map(str::trim)
        .filter(|t| !t.is_empty() && !matches!(*t, "[Private video]" | "[Deleted video]"))
        .map(str::to_string);
    let nested_playlist = entry.get("_type").and_then(|t| t.as_str()) == Some("playlist")
        || matches!(entry.get("ie_key").and_then(|k| k.as_str()), Some("YoutubeTab" | "YoutubePlaylist"));
    PlaylistEntry {
        position,
        url: entry_url(entry),
        title,
        description: entry.get("description").and_then(|d| d.as_str()).unwrap_or("").to_string(),
        is_live: is_live_json(entry),
        nested_playlist,
    }
}

pub fn parse_probe(json: &serde_json::Value) -> Probe {
    if let Some(entries) = json.get("entries").and_then(|e| e.as_array()) {
        let parsed = entries
            .iter()
            .take(MAX_PLAYLIST_ENTRIES)
            .enumerate()
            .map(|(i, entry)| parse_playlist_entry(i + 1, entry))
            .collect();
        return Probe::Playlist {
            title: json.get("title").and_then(|t| t.as_str()).unwrap_or("").to_string(),
            entries: parsed,
            truncated_from: (entries.len() > MAX_PLAYLIST_ENTRIES).then_some(entries.len()),
        };
    }
    if json.get("_type").and_then(|t| t.as_str()) == Some("playlist") {
        return Probe::Unknown;
    }
    match json.get("title").and_then(|t| t.as_str()) {
        Some(title) => Probe::Single(VideoInfo {
            title: title.to_string(),
            description: json.get("description").and_then(|d| d.as_str()).unwrap_or("").to_string(),
            is_live: is_live_json(json),
        }),
        None => Probe::Unknown,
    }
}

// YouTube tacks a `list=RD...` (and usually `start_radio=1`) onto a video
// URL whenever an autoplay mix was running when the link was copied. That
// mix is auto-generated and usually not what someone meant to add by
// pasting "this video"; a real user-curated playlist uses `list=PL...` (or
// other non-RD prefixes) and is left alone. Stripping it here makes such a
// link probe and classify as the single video, matching what copying a
// YouTube link while a mix plays actually means in practice.
pub fn strip_auto_mix_list(source: &str) -> String {
    let Some((base, query)) = source.split_once('?') else {
        return source.to_string();
    };
    let kept: Vec<&str> = query
        .split('&')
        .filter(|param| {
            let is_auto_mix_list = param
                .strip_prefix("list=")
                .is_some_and(|value| value.starts_with("RD"));
            !is_auto_mix_list && !param.starts_with("start_radio=")
        })
        .collect();
    if kept.is_empty() {
        base.to_string()
    } else {
        format!("{base}?{}", kept.join("&"))
    }
}

// Callers must have ruled out local sources first: this always runs yt-dlp.
// Callers are expected to have already applied strip_auto_mix_list, since
// the cleaned URL is also what should end up stored as the source.
pub fn probe_url(source: &str) -> Probe {
    match run_yt_dlp_json(&["--flat-playlist", "--dump-single-json", "--no-warnings"], source) {
        Ok(json) => parse_probe(&json),
        Err(_) => Probe::Unknown,
    }
}

// The single-video fallback when the playlist probe could not tell; prints
// the same message classify_source does on failure.
pub fn fetch_video_info(source: &str) -> Option<VideoInfo> {
    match fetch_yt_dlp_metadata(source) {
        Ok(json) => match parse_probe(&json) {
            Probe::Single(info) => Some(info),
            _ => None,
        },
        Err(message) => {
            eprintln!("{message}");
            None
        }
    }
}

// yt-dlp goes quiet whenever --print is used, and --progress (needed to get
// progress back) writes to stdout, the same stream the saved path is printed
// on. The marker tells the two apart.
const SAVED_PATH_MARKER: &str = "LOFI-SAVED:";
const MAX_DOWNLOAD_LINE_BYTES: u64 = 64 * 1024;
const MAX_SAVED_PATHS: usize = 16;

#[derive(Debug, PartialEq, Eq)]
pub enum DownloadLine<'a> {
    Saved(&'a str),
    Progress(&'a str),
    Other(&'a str),
}

pub fn classify_download_line(line: &str) -> DownloadLine<'_> {
    if let Some(path) = line.strip_prefix(SAVED_PATH_MARKER) {
        DownloadLine::Saved(path)
    } else if line.starts_with("[download]") {
        DownloadLine::Progress(line)
    } else {
        DownloadLine::Other(line)
    }
}

// The printed path is trusted only as far as being a regular file directly
// inside the directory yt-dlp was told to save into.
pub fn validate_saved_path(printed: &str, canonical_dir: &Path) -> Option<String> {
    let path = Path::new(printed);
    if !path.is_absolute() {
        return None;
    }
    let meta = std::fs::symlink_metadata(path).ok()?;
    if !meta.file_type().is_file() {
        return None;
    }
    let parent = std::fs::canonicalize(path.parent()?).ok()?;
    if parent != canonical_dir {
        return None;
    }
    parent.join(path.file_name()?).into_os_string().into_string().ok()
}

#[derive(Debug, PartialEq, Eq)]
pub struct DownloadOutcome {
    pub saved: Vec<String>,
    pub exited_ok: bool,
}

struct ProgressLine {
    enabled: bool,
    width: usize,
}

impl ProgressLine {
    fn show(&mut self, text: &str) {
        if self.enabled {
            let width = self.width;
            eprint!("\r{text:<width$}");
            self.width = text.chars().count();
        }
    }

    fn finish(&mut self) {
        if self.width > 0 {
            eprintln!();
            self.width = 0;
        }
    }
}

// Callers must have ruled out local sources first: this always runs yt-dlp.
pub fn download_audio(
    source: &str,
    dest_dir: &Path,
    filename_template: &str,
    format: &str,
) -> anyhow::Result<DownloadOutcome> {
    let canonical_dir = std::fs::canonicalize(dest_dir)
        .map_err(|e| anyhow::anyhow!("could not use download directory {}: {e}", dest_dir.display()))?;
    let mut child = std::process::Command::new(yt_dlp_binary())
        .arg("-f")
        .arg(format)
        .args(["--restrict-filenames", "--no-warnings", "--no-playlist", "--progress", "--newline"])
        .arg("--print")
        .arg(format!("after_move:{SAVED_PATH_MARKER}%(filepath)s"))
        .args(["--socket-timeout", "10"])
        // -P rather than a directory inside -o, so a "%" in the path is not
        // read as a template field.
        .arg("-P")
        .arg(&canonical_dir)
        .arg("-o")
        .arg(filename_template)
        .arg("--")
        .arg(source)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::inherit())
        .spawn()
        .map_err(|e| anyhow::anyhow!("could not run yt-dlp to download '{source}': {e}"))?;

    use std::io::IsTerminal;
    let mut progress = ProgressLine { enabled: std::io::stderr().is_terminal(), width: 0 };
    let mut saved = Vec::new();
    if let Some(stdout) = child.stdout.take() {
        let mut reader = BufReader::new(stdout);
        loop {
            let mut buf = Vec::new();
            match (&mut reader).take(MAX_DOWNLOAD_LINE_BYTES).read_until(b'\n', &mut buf) {
                Ok(0) | Err(_) => break,
                Ok(_) => {}
            }
            let line = String::from_utf8_lossy(&buf);
            let line = line.trim_end_matches(['\n', '\r']);
            match classify_download_line(line) {
                DownloadLine::Saved(printed) => {
                    progress.finish();
                    match validate_saved_path(printed, &canonical_dir) {
                        Some(path) if saved.len() < MAX_SAVED_PATHS => saved.push(path),
                        _ => eprintln!("warning: ignoring unexpected saved path from yt-dlp: {}", sanitize_for_display(printed)),
                    }
                }
                DownloadLine::Progress(text) => progress.show(text),
                DownloadLine::Other(text) => {
                    progress.finish();
                    eprintln!("{text}");
                }
            }
        }
    }
    let status = child.wait()?;
    progress.finish();
    Ok(DownloadOutcome { saved, exited_ok: status.success() })
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
pub(crate) mod test_support {
    pub(crate) fn env_lock() -> std::sync::MutexGuard<'static, ()> {
        crate::ENV_MUTEX.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    pub(crate) fn fake_yt_dlp_script(dir: &std::path::Path, stdout: &str) -> std::path::PathBuf {
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

    pub(crate) fn recorded_yt_dlp_args(dir: &std::path::Path) -> Vec<String> {
        std::fs::read_to_string(dir.join("yt-dlp-args"))
            .unwrap()
            .lines()
            .map(str::to_string)
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use super::test_support::*;
    use std::io::{BufRead, BufReader, Write};
    use std::os::unix::net::UnixListener;

    #[test]
    fn strip_auto_mix_list_removes_an_auto_generated_mix_marker() {
        let url = "https://www.youtube.com/watch?v=w4TNGhSj2tc&list=RDw4TNGhSj2tc&start_radio=1";
        assert_eq!(
            strip_auto_mix_list(url),
            "https://www.youtube.com/watch?v=w4TNGhSj2tc"
        );
    }

    #[test]
    fn strip_auto_mix_list_leaves_a_real_playlist_alone() {
        let url = "https://www.youtube.com/watch?v=abc123&list=PLsomeUserCuratedPlaylist";
        assert_eq!(strip_auto_mix_list(url), url);
    }

    #[test]
    fn strip_auto_mix_list_leaves_a_url_with_no_query_alone() {
        let url = "https://www.youtube.com/watch?v=abc123";
        assert_eq!(strip_auto_mix_list(url), url);
    }

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

    // Trimmed from a real `yt-dlp --flat-playlist --dump-single-json` of a
    // YouTube playlist (yt-dlp 2026.08.19), including a private video's stub.
    const SAMPLE_FLAT_PLAYLIST_JSON: &str = r#"{
        "id": "PLgouNNTJjiigY4HZ9BQuMPioOmJk_bans",
        "title": "Rainy Night Lofi",
        "description": "",
        "_type": "playlist",
        "extractor_key": "YoutubeTab",
        "entries": [
            {"_type": "url", "ie_key": "Youtube", "id": "i778hCmt5gk",
             "url": "https://www.youtube.com/watch?v=i778hCmt5gk",
             "title": "Just relax and fall asleep - Lofi Hip Hop Mix", "duration": 4298, "live_status": null},
            {"_type": "url", "ie_key": "Youtube", "id": "l6_lDBspku4",
             "url": "https://www.youtube.com/watch?v=l6_lDBspku4",
             "title": null, "duration": null, "live_status": null},
            {"_type": "url", "ie_key": "Youtube", "id": "Uo7AK05SVpk",
             "title": "3 Hours Of Rain Sounds On Window", "live_status": "is_live"}
        ]
    }"#;

    // Trimmed from a real flat dump of a single watch URL: the full video
    // metadata, with no entries array.
    const SAMPLE_FLAT_SINGLE_VIDEO_JSON: &str = r#"{
        "id": "dQw4w9WgXcQ",
        "title": "Heavy rain lofi",
        "description": "a long description",
        "_type": "video",
        "is_live": false,
        "webpage_url": "https://www.youtube.com/watch?v=dQw4w9WgXcQ"
    }"#;

    #[test]
    fn parse_probe_recognizes_a_flat_playlist_and_its_entries() {
        let json: serde_json::Value = serde_json::from_str(SAMPLE_FLAT_PLAYLIST_JSON).unwrap();
        let Probe::Playlist { title, entries, truncated_from } = parse_probe(&json) else {
            panic!("not detected as a playlist");
        };
        assert_eq!(title, "Rainy Night Lofi");
        assert_eq!(truncated_from, None);
        assert_eq!(entries.len(), 3);
        assert_eq!(entries[0].position, 1);
        assert_eq!(entries[0].url.as_deref(), Some("https://www.youtube.com/watch?v=i778hCmt5gk"));
        assert_eq!(entries[0].title.as_deref(), Some("Just relax and fall asleep - Lofi Hip Hop Mix"));
        assert!(!entries[0].is_live && !entries[0].nested_playlist);
        assert_eq!(entries[1].title, None, "a private video's null title must not look classifiable");
        // No "url" field: rebuilt from the id.
        assert_eq!(entries[2].url.as_deref(), Some("https://www.youtube.com/watch?v=Uo7AK05SVpk"));
        assert!(entries[2].is_live);
    }

    #[test]
    fn parse_probe_recognizes_a_single_video() {
        let json: serde_json::Value = serde_json::from_str(SAMPLE_FLAT_SINGLE_VIDEO_JSON).unwrap();
        assert_eq!(
            parse_probe(&json),
            Probe::Single(VideoInfo {
                title: "Heavy rain lofi".to_string(),
                description: "a long description".to_string(),
                is_live: false,
            })
        );
        let live: serde_json::Value =
            serde_json::from_str(r#"{"title": "lofi radio", "live_status": "is_live"}"#).unwrap();
        assert!(matches!(parse_probe(&live), Probe::Single(VideoInfo { is_live: true, .. })));
    }

    #[test]
    fn parse_probe_returns_unknown_for_unrecognized_shapes() {
        for json in [
            r#"{}"#,
            r#"{"_type": "url", "url": "https://example.com/x"}"#,
            r#"{"_type": "playlist", "title": "no entries array"}"#,
            r#"{"_type": "playlist", "entries": "nope"}"#,
            r#"[1, 2]"#,
        ] {
            assert_eq!(parse_probe(&serde_json::from_str(json).unwrap()), Probe::Unknown, "{json}");
        }
    }

    #[test]
    fn parse_probe_flags_nested_playlists_and_unbuildable_urls() {
        let json: serde_json::Value = serde_json::from_str(
            r#"{"_type": "playlist", "title": "search", "entries": [
                {"_type": "url", "ie_key": "YoutubeTab", "id": "PLxyz",
                 "url": "https://www.youtube.com/playlist?list=PLxyz", "title": "Rainy lofi"},
                {"_type": "url", "ie_key": "Vimeo", "id": "12345", "url": "12345", "title": "rain"},
                {"_type": "url", "id": "bad id with spaces", "title": "rain"},
                {"_type": "url", "ie_key": "Youtube", "id": "abcdefghijk", "title": "[Deleted video]"}
            ]}"#,
        )
        .unwrap();
        let Probe::Playlist { entries, .. } = parse_probe(&json) else { panic!() };
        assert!(entries[0].nested_playlist);
        assert_eq!(entries[1].url, None);
        assert_eq!(entries[2].url, None);
        assert_eq!(entries[3].title, None);
        assert_eq!(entries[3].url.as_deref(), Some("https://www.youtube.com/watch?v=abcdefghijk"));
    }

    #[test]
    fn parse_probe_caps_the_number_of_playlist_entries() {
        let entries: Vec<String> = (0..MAX_PLAYLIST_ENTRIES + 5)
            .map(|i| format!(r#"{{"id": "id{i:09}", "title": "t"}}"#))
            .collect();
        let json: serde_json::Value =
            serde_json::from_str(&format!(r#"{{"_type": "playlist", "entries": [{}]}}"#, entries.join(","))).unwrap();
        let Probe::Playlist { entries, truncated_from, .. } = parse_probe(&json) else { panic!() };
        assert_eq!(entries.len(), MAX_PLAYLIST_ENTRIES);
        assert_eq!(truncated_from, Some(MAX_PLAYLIST_ENTRIES + 5));
    }

    #[test]
    fn probe_url_uses_a_flat_single_document_dump_after_a_separator() {
        let _guard = env_lock();
        let dir = tempfile::tempdir().unwrap();
        let yt_dlp = fake_yt_dlp_script(dir.path(), SAMPLE_FLAT_PLAYLIST_JSON);
        std::env::set_var("LOFI_YTDLP_BIN", &yt_dlp);
        let probe = probe_url("https://www.youtube.com/playlist?list=PLxyz");
        std::env::remove_var("LOFI_YTDLP_BIN");
        assert!(matches!(probe, Probe::Playlist { .. }));
        let args = recorded_yt_dlp_args(dir.path());
        let separator = args.iter().position(|a| a == "--").expect("missing -- separator");
        let options = &args[..separator];
        for flag in ["--flat-playlist", "--dump-single-json", "--no-warnings", "--socket-timeout"] {
            assert!(options.contains(&flag.to_string()), "missing {flag}: {args:?}");
        }
        assert!(!options.contains(&"--no-playlist".to_string()));
        assert_eq!(args[separator + 1], "https://www.youtube.com/playlist?list=PLxyz");
    }

    #[test]
    fn probe_url_degrades_to_unknown_when_yt_dlp_is_missing_or_output_is_garbage() {
        let _guard = env_lock();
        std::env::set_var("LOFI_YTDLP_BIN", "/nonexistent/definitely-not-yt-dlp");
        let missing = probe_url("https://example.com/some-video");
        let dir = tempfile::tempdir().unwrap();
        let yt_dlp = fake_yt_dlp_script(dir.path(), "not json at all");
        std::env::set_var("LOFI_YTDLP_BIN", &yt_dlp);
        let garbage = probe_url("https://example.com/some-video");
        std::env::remove_var("LOFI_YTDLP_BIN");
        assert_eq!(missing, Probe::Unknown);
        assert_eq!(garbage, Probe::Unknown);
    }

    #[test]
    fn download_lines_are_split_into_saved_paths_progress_and_everything_else() {
        assert_eq!(classify_download_line("LOFI-SAVED:/data/a.m4a"), DownloadLine::Saved("/data/a.m4a"));
        assert_eq!(
            classify_download_line("[download]  42.0% of 1.25MiB"),
            DownloadLine::Progress("[download]  42.0% of 1.25MiB")
        );
        assert_eq!(classify_download_line("/data/a.m4a"), DownloadLine::Other("/data/a.m4a"));
    }

    #[test]
    fn validate_saved_path_accepts_only_regular_files_directly_inside_the_target_dir() {
        let dir = tempfile::tempdir().unwrap();
        let target = std::fs::canonicalize(dir.path()).unwrap();
        std::fs::create_dir(target.join("sub")).unwrap();
        std::fs::write(target.join("a.m4a"), b"x").unwrap();
        std::fs::write(target.join("sub/b.m4a"), b"x").unwrap();
        std::os::unix::fs::symlink("/etc/passwd", target.join("link.m4a")).unwrap();
        let good = target.join("a.m4a");
        assert_eq!(validate_saved_path(good.to_str().unwrap(), &target), Some(good.to_str().unwrap().to_string()));
        for bad in [
            target.join("sub/b.m4a").to_str().unwrap().to_string(),
            target.join("link.m4a").to_str().unwrap().to_string(),
            target.join("missing.m4a").to_str().unwrap().to_string(),
            target.join("sub").to_str().unwrap().to_string(),
            "a.m4a".to_string(),
            "/etc/passwd".to_string(),
        ] {
            assert_eq!(validate_saved_path(&bad, &target), None, "{bad}");
        }
    }

    // Simulates a download: saves "<-P dir>/<-o template, filled in>" and
    // prints the marker line, or fails for URLs containing "fail".
    fn fake_downloading_yt_dlp(dir: &std::path::Path, exit_code: i32) -> std::path::PathBuf {
        use std::os::unix::fs::PermissionsExt;
        let path = dir.join("fake-yt-dlp-download");
        let script = format!(
            r#"#!/bin/sh
for a; do printf '%s\n' "$a"; done > '{args}'
prev=""; for a; do case "$prev" in -P) out="$a";; -o) tmpl="$a";; esac; prev="$a"; done
case "$prev" in *fail*) echo "ERROR: Private video" >&2; exit 1;; esac
name=$(printf '%s' "$tmpl" | sed 's/%(title)s/Some_Title/; s/%(ext)s/m4a/')
echo "[download]  50.0% of 1.00MiB"
: > "$out/$name"
echo "LOFI-SAVED:$out/$name"
echo "LOFI-SAVED:/etc/passwd"
exit {exit_code}
"#,
            args = dir.join("yt-dlp-args").display()
        );
        std::fs::write(&path, script).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        path
    }

    #[test]
    fn download_audio_returns_the_validated_saved_path_and_passes_safe_arguments() {
        let _guard = env_lock();
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("rainy-day");
        std::fs::create_dir(&target).unwrap();
        std::env::set_var("LOFI_YTDLP_BIN", fake_downloading_yt_dlp(dir.path(), 0));
        let outcome = download_audio("https://www.youtube.com/watch?v=abc", &target, "%(title)s.%(ext)s", "bestaudio/worst");
        std::env::remove_var("LOFI_YTDLP_BIN");
        let expected = std::fs::canonicalize(&target).unwrap().join("Some_Title.m4a");
        assert_eq!(
            outcome.unwrap(),
            DownloadOutcome { saved: vec![expected.to_str().unwrap().to_string()], exited_ok: true },
            "the out-of-directory path must be ignored"
        );
        let args = recorded_yt_dlp_args(dir.path());
        let separator = args.iter().position(|a| a == "--").expect("missing -- separator");
        assert_eq!(args[separator + 1], "https://www.youtube.com/watch?v=abc");
        let options = &args[..separator];
        let value_of = |flag: &str| options[options.iter().position(|a| a == flag).unwrap() + 1].clone();
        assert_eq!(value_of("-f"), "bestaudio/worst");
        assert_eq!(value_of("-o"), "%(title)s.%(ext)s");
        assert_eq!(value_of("--print"), "after_move:LOFI-SAVED:%(filepath)s");
        for flag in ["--restrict-filenames", "--no-warnings", "--no-playlist", "--progress"] {
            assert!(options.contains(&flag.to_string()), "missing {flag}: {args:?}");
        }
    }

    #[test]
    fn download_audio_keeps_a_saved_path_even_when_yt_dlp_then_fails() {
        let _guard = env_lock();
        let dir = tempfile::tempdir().unwrap();
        std::env::set_var("LOFI_YTDLP_BIN", fake_downloading_yt_dlp(dir.path(), 1));
        let partial = download_audio("https://example.com/v", dir.path(), "x.%(ext)s", "worstaudio/worst").unwrap();
        let failed = download_audio("https://example.com/fail", dir.path(), "y.%(ext)s", "worstaudio/worst").unwrap();
        std::env::remove_var("LOFI_YTDLP_BIN");
        assert_eq!(partial.saved.len(), 1);
        assert!(!partial.exited_ok);
        assert_eq!(failed, DownloadOutcome { saved: vec![], exited_ok: false });
    }

    #[test]
    fn download_audio_reports_a_missing_yt_dlp_as_an_error() {
        let _guard = env_lock();
        let dir = tempfile::tempdir().unwrap();
        std::env::set_var("LOFI_YTDLP_BIN", "/nonexistent/definitely-not-yt-dlp");
        let result = download_audio("https://example.com/v", dir.path(), "x.%(ext)s", "worstaudio/worst");
        std::env::remove_var("LOFI_YTDLP_BIN");
        assert!(result.is_err());
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
