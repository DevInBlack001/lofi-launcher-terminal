use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

pub trait MpvController {
    // Convenience wrapper that RealMpv's own start_source delegates through for the
    // simple case; nothing in the current binary calls it directly since everything
    // goes through start_source_with_duration now.
    #[allow(dead_code)]
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
    // Part of the trait contract for future error-surfacing use; not yet read anywhere.
    #[allow(dead_code)]
    fn last_error(&self) -> Option<String>;
    fn quit(&mut self) -> anyhow::Result<()>;
}

pub struct RealMpv {
    socket_path: PathBuf,
    child: Child,
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
    let base = std::env::var("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .or_else(|_| std::env::var("HOME").map(|home| PathBuf::from(home).join(".config")))
        .ok()?;
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
        self.send_with_timeout(payload, Duration::from_secs(2))
    }

    fn send_with_timeout(
        &self,
        payload: serde_json::Value,
        read_timeout: Duration,
    ) -> anyhow::Result<serde_json::Value> {
        let mut stream = UnixStream::connect(&self.socket_path)?;
        let mut line = serde_json::to_string(&payload)?;
        line.push('\n');
        stream.write_all(line.as_bytes())?;
        stream.set_read_timeout(Some(read_timeout))?;
        let mut reader = BufReader::new(stream);
        let mut response = String::new();
        reader.read_line(&mut response)?;
        Ok(serde_json::from_str(&response)?)
    }

    // mpv needs a moment to resolve a loaded file's real duration (network sources
    // in particular), so poll a few times rather than trusting the first reply.
    // This runs while DaemonState's mutex is held, so each attempt uses a short
    // timeout instead of the normal 2s: a stalled source should cap the whole
    // poll at a few seconds, not block every other daemon command for ~42s.
    fn query_duration(&self) -> Option<u64> {
        for _ in 0..20 {
            if let Ok(resp) = self.send_with_timeout(
                serde_json::json!({ "command": ["get_property", "duration"] }),
                Duration::from_millis(250),
            ) {
                if let Some(data) = resp.get("data") {
                    if let Some(secs) = data.as_f64() {
                        if secs > 0.0 {
                            return Some(secs as u64);
                        }
                    }
                }
            }
            std::thread::sleep(Duration::from_millis(100));
        }
        None
    }
}

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
        // In real usage the daemon never knows the duration up front, so when the
        // caller didn't supply one, ask mpv itself after the file has loaded.
        let effective_duration = duration_seconds.or_else(|| self.query_duration());
        if let Some(duration) = effective_duration {
            if duration > long_source_threshold_seconds {
                let offset = rand_offset_seconds(duration);
                self.send(serde_json::json!({ "command": ["set_property", "time-pos", offset] }))?;
                self.send(serde_json::json!({ "command": ["set_property", "loop-file", "inf"] }))?;
            }
        }
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

    fn quit(&mut self) -> anyhow::Result<()> {
        self.send(serde_json::json!({ "command": ["quit"] }))?;
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
    }
}

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
