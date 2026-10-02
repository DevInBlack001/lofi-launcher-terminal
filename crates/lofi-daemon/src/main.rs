use lofi_daemon::mpv::{MpvController, RealMpv};
use lofi_daemon::server;
use lofi_daemon::state::DaemonState;
use std::os::unix::fs::OpenOptionsExt;
use std::sync::{Arc, Mutex};

fn runtime_dir() -> std::path::PathBuf {
    std::env::var_os("XDG_RUNTIME_DIR")
        .map(std::path::PathBuf::from)
        .expect("XDG_RUNTIME_DIR must be set; lofi-daemon targets Linux session environments")
}

// Several terminals opening at once each spawn a daemon. Without a single
// instance lock they race on the shared socket path and each spawns its own
// mpv, orphaning all but the last.
fn acquire_single_instance_lock(run_dir: &std::path::Path) -> anyhow::Result<Option<std::fs::File>> {
    use fs2::FileExt;
    let lock_path = run_dir.join("lofi-daemon.lock");
    let file = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW)
        .open(&lock_path)
        .map_err(|e| anyhow::anyhow!("could not open lock file {}: {e}", lock_path.display()))?;
    match file.try_lock_exclusive() {
        Ok(()) => Ok(Some(file)),
        Err(e) if e.raw_os_error() == fs2::lock_contended_error().raw_os_error() => Ok(None),
        Err(e) => Err(anyhow::anyhow!("could not lock {}: {e}", lock_path.display())),
    }
}

fn main() -> anyhow::Result<()> {
    let run_dir = runtime_dir();
    // Held for the whole process lifetime; the kernel releases it on exit.
    let _instance_lock = match acquire_single_instance_lock(&run_dir)? {
        Some(lock) => lock,
        None => {
            eprintln!("another lofi-daemon is already running");
            std::process::exit(0);
        }
    };

    let config = lofi_common::load_config_or_default(&lofi_common::config_path())?;

    let mpv_binary_check = std::process::Command::new(std::env::var("LOFI_MPV_BIN").unwrap_or_else(|_| "mpv".to_string()))
        .arg("--version")
        .output();
    if mpv_binary_check.is_err() {
        eprintln!("mpv not detected, install it via your distro's package manager");
        std::process::exit(1);
    }

    let mpv_socket = run_dir.join("lofi-mpv.sock");
    let daemon_socket = run_dir.join("lofi-daemon.sock");

    let mpv = RealMpv::spawn(mpv_socket)?;
    let state = Arc::new(Mutex::new(DaemonState::new(config, mpv)));

    // A raw SIGTERM/SIGINT skips Drop, so without this handler mpv is
    // orphaned (reparented to init) and keeps playing after the daemon dies.
    let shutdown_state = state.clone();
    ctrlc::set_handler(move || {
        // A poisoned lock's data is still usable; recovering it here matters
        // because skipping quit() on poison would silently reproduce the
        // orphaned-mpv bug this handler exists to prevent.
        let mut guard = shutdown_state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let _ = guard.mpv.quit();
        std::process::exit(0);
    })
    .expect("failed to install signal handler");

    server::serve(&daemon_socket, state)
}
