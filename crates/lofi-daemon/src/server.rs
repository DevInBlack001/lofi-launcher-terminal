use crate::mpv::MpvController;
use crate::state::DaemonState;
use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::{UnixListener, UnixStream};
use std::sync::{Arc, Mutex};

pub fn bind(socket_path: &std::path::Path) -> anyhow::Result<UnixListener> {
    if socket_path.exists() {
        std::fs::remove_file(socket_path)?;
    }
    Ok(UnixListener::bind(socket_path)?)
}

pub fn serve<M: MpvController>(
    socket_path: &std::path::Path,
    state: Arc<Mutex<DaemonState<M>>>,
) -> anyhow::Result<()> {
    serve_listener(bind(socket_path)?, state)
}

pub fn serve_listener<M: MpvController>(
    listener: UnixListener,
    state: Arc<Mutex<DaemonState<M>>>,
) -> anyhow::Result<()> {
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
        fn start_source_with_duration(
            &mut self,
            _source: &str,
            _duration_seconds: Option<u64>,
            _long_source_threshold_seconds: u64,
        ) -> anyhow::Result<()> { Ok(()) }
        fn stop(&mut self) -> anyhow::Result<()> { Ok(()) }
        fn pause(&mut self) -> anyhow::Result<()> { Ok(()) }
        fn resume(&mut self) -> anyhow::Result<()> { Ok(()) }
        fn last_error(&self) -> Option<String> { None }
        fn quit(&mut self) -> anyhow::Result<()> { Ok(()) }
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
