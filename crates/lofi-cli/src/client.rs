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

pub fn ensure_daemon_running(socket_path: &Path) -> anyhow::Result<()> {
    if UnixStream::connect(socket_path).is_ok() {
        return Ok(());
    }
    // A stale socket file is left for the daemon to replace: it only does so
    // after taking its single-instance lock, whereas deleting it here could
    // unlink the socket of a daemon another terminal just started.
    std::process::Command::new(daemon_binary())
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()?;

    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    while UnixStream::connect(socket_path).is_err() {
        if std::time::Instant::now() > deadline {
            anyhow::bail!("lofi-daemon did not start within 5 seconds");
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    Ok(())
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

    let config_path = lofi_common::config_path();
    let config = lofi_common::load_config(&config_path)?;
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

    #[test]
    fn daemon_binary_respects_env_override() {
        std::env::set_var("LOFI_DAEMON_BIN", "/tmp/some-custom-lofi-daemon");
        let resolved = daemon_binary();
        std::env::remove_var("LOFI_DAEMON_BIN");
        assert_eq!(resolved, std::path::PathBuf::from("/tmp/some-custom-lofi-daemon"));
    }

    #[test]
    fn classify_source_degrades_gracefully_when_yt_dlp_is_missing() {
        std::env::set_var("LOFI_YTDLP_BIN", "/nonexistent/definitely-not-yt-dlp");
        let result = classify_source("https://example.com/some-video");
        std::env::remove_var("LOFI_YTDLP_BIN");
        assert_eq!(result.unwrap(), None);
    }
}
