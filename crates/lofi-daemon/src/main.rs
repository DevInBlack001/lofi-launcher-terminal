mod mpv;
mod server;
mod state;

use mpv::{MpvController, RealMpv};
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

    // A raw SIGTERM/SIGINT skips Drop, so without this handler mpv is
    // orphaned (reparented to init) and keeps playing after the daemon dies.
    let shutdown_state = state.clone();
    ctrlc::set_handler(move || {
        if let Ok(mut guard) = shutdown_state.lock() {
            let _ = guard.mpv.quit();
        }
        std::process::exit(0);
    })
    .expect("failed to install signal handler");

    server::serve(&daemon_socket, state)
}
