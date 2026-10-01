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
