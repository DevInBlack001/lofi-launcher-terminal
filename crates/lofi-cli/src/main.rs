mod client;

use clap::{Parser, Subcommand, ValueEnum};
use lofi_common::Command as DaemonCommand;

#[derive(Parser)]
#[command(name = "lofi")]
#[command(about = "Play mood-based lofi music in the background, controlled from the terminal")]
struct Cli {
    #[command(subcommand)]
    command: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Called by shell integration when a terminal/TTY opens; starts playback on the first session
    Register,
    /// Called by shell integration when a terminal/TTY closes; stops playback when the last session ends
    Unregister,
    /// Switch the active mood (and its source list)
    Mood { name: String },
    /// Skip to the next source in the current mood
    Next,
    /// Pause playback without ending the session
    Pause,
    /// Resume playback without ending the session
    Resume,
    /// Show the current mood, playing/paused state, and current source
    Status,
    /// List the configured mood names
    Moods,
    /// Re-read config.toml from disk without restarting the daemon or stopping playback
    Reload,
    /// Turn auto-advance on or off: when on, a finished source moves on to the next one in the mood
    Loop { state: OnOff },
    /// Pick audio-only stream quality for URL sources (min saves bandwidth); applies from the next source
    Quality { level: QualityLevel },
    /// Open an interactive mood picker
    Tui,
    /// Classify a source into a mood (or use an explicit mood) and add it to the config
    Add {
        source: String,
        #[arg(long)]
        mood: Option<String>,
    },
}

#[derive(Clone, Copy, ValueEnum)]
enum OnOff {
    On,
    Off,
}

#[derive(Clone, Copy, ValueEnum)]
enum QualityLevel {
    Min,
    Max,
}

impl QualityLevel {
    fn as_str(self) -> &'static str {
        match self {
            QualityLevel::Min => "min",
            QualityLevel::Max => "max",
        }
    }
}

pub(crate) fn quality_label(quality: &str) -> &str {
    if quality.is_empty() {
        "unknown"
    } else {
        quality
    }
}

fn runtime_socket() -> std::path::PathBuf {
    let run_dir = lofi_common::xdg_absolute_dir("XDG_RUNTIME_DIR")
        .expect("XDG_RUNTIME_DIR must be set to an absolute path; lofi targets Linux session environments");
    run_dir.join("lofi-daemon.sock")
}

pub(crate) fn on_off_label(enabled: bool) -> &'static str {
    if enabled {
        "on"
    } else {
        "off"
    }
}

pub(crate) fn playback_state_label(playing: bool, paused: bool) -> &'static str {
    if playing {
        "playing"
    } else if paused {
        "paused"
    } else {
        "stopped"
    }
}

fn print_response(resp: lofi_common::Response) {
    match resp {
        lofi_common::Response::Ok => println!("ok"),
        lofi_common::Response::Error(msg) => eprintln!("error: {msg}"),
        lofi_common::Response::Status { mood, playing, paused, current_source, loop_playback, audio_quality } => {
            let state = playback_state_label(playing, paused);
            let source = current_source.unwrap_or_else(|| "none".to_string());
            let looping = on_off_label(loop_playback);
            let quality = quality_label(&audio_quality);
            println!("mood: {mood}\nstate: {state}\nsource: {source}\nloop: {looping}\nquality: {quality}");
        }
        lofi_common::Response::Moods(names) => {
            for name in names {
                println!("{name}");
            }
        }
        lofi_common::Response::Classified(mood) => println!("added to mood: {mood}"),
    }
}

fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();
    let socket = runtime_socket();

    client::ensure_daemon_running(&socket)?;

    if matches!(cli.command, Cmd::Tui) {
        return tui::run(socket);
    }

    if let Cmd::Add { source, mood } = &cli.command {
        let source = client::resolve_source(source)?;
        let resolved_mood = match mood {
            Some(m) => Some(m.clone()),
            None => client::classify_source(&source)?,
        };
        if resolved_mood.is_none() {
            eprintln!(
                "could not classify '{source}' into a mood automatically; re-run with --mood <name>"
            );
            std::process::exit(1);
        }
        let resp = client::send_command(
            &socket,
            &DaemonCommand::Add { source, mood: resolved_mood },
        )?;
        print_response(resp);
        return Ok(());
    }

    client::ensure_daemon_running(&socket)?;

    let cmd = match cli.command {
        Cmd::Register => DaemonCommand::Register,
        Cmd::Unregister => DaemonCommand::Unregister,
        Cmd::Mood { name } => DaemonCommand::Mood(name),
        Cmd::Next => DaemonCommand::Next,
        Cmd::Pause => DaemonCommand::Pause,
        Cmd::Resume => DaemonCommand::Resume,
        Cmd::Status => DaemonCommand::Status,
        Cmd::Moods => DaemonCommand::Moods,
        Cmd::Reload => DaemonCommand::Reload,
        Cmd::Loop { state } => DaemonCommand::SetLoop(matches!(state, OnOff::On)),
        Cmd::Quality { level } => DaemonCommand::SetAudioQuality(level.as_str().to_string()),
        Cmd::Tui => unreachable!("handled above"),
        Cmd::Add { .. } => unreachable!("handled above"),
    };

    let resp = client::send_command(&socket, &cmd)?;
    print_response(resp);
    Ok(())
}

mod tui;

// One lock for every test in this crate that mutates process-wide env vars,
// since tests run on parallel threads in one process.
#[cfg(test)]
pub(crate) static ENV_MUTEX: std::sync::Mutex<()> = std::sync::Mutex::new(());

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn playback_state_label_distinguishes_all_three_states() {
        assert_eq!(playback_state_label(true, false), "playing");
        assert_eq!(playback_state_label(false, true), "paused");
        assert_eq!(playback_state_label(false, false), "stopped");
    }

    #[test]
    fn loop_subcommand_accepts_only_on_or_off() {
        assert!(matches!(Cli::try_parse_from(["lofi", "loop", "on"]).unwrap().command, Cmd::Loop { state: OnOff::On }));
        assert!(matches!(Cli::try_parse_from(["lofi", "loop", "off"]).unwrap().command, Cmd::Loop { state: OnOff::Off }));
        assert!(Cli::try_parse_from(["lofi", "loop", "maybe"]).is_err());
        assert!(Cli::try_parse_from(["lofi", "loop"]).is_err());
    }

    #[test]
    fn quality_subcommand_accepts_only_min_or_max() {
        for (arg, expected) in [("min", "min"), ("max", "max")] {
            match Cli::try_parse_from(["lofi", "quality", arg]).unwrap().command {
                Cmd::Quality { level } => assert_eq!(level.as_str(), expected),
                _ => panic!("parsed as the wrong subcommand"),
            }
        }
        assert!(Cli::try_parse_from(["lofi", "quality", "medium"]).is_err());
        assert_eq!(quality_label(""), "unknown");
    }
}
