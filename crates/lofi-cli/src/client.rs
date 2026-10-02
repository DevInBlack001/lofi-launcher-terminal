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

    // Package installs never seed config.toml, so fall back to the shipped
    // default classifier keywords rather than failing on a missing file.
    let config = lofi_common::load_config_or_default(&lofi_common::config_path())?;
    Ok(lofi_common::classify(&config.classifier, title, description))
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
