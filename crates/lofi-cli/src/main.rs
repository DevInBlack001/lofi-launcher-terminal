mod client;

use clap::{Parser, Subcommand};
use lofi_common::Command as DaemonCommand;

#[derive(Parser)]
#[command(name = "lofi")]
struct Cli {
    #[command(subcommand)]
    command: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    Register,
    Unregister,
    Mood { name: String },
    Next,
    Pause,
    Resume,
    Status,
    Moods,
    Tui,
    Add {
        source: String,
        #[arg(long)]
        mood: Option<String>,
    },
}

fn runtime_socket() -> std::path::PathBuf {
    let run_dir = std::env::var_os("XDG_RUNTIME_DIR")
        .map(std::path::PathBuf::from)
        .expect("XDG_RUNTIME_DIR must be set; lofi targets Linux session environments");
    run_dir.join("lofi-daemon.sock")
}

fn print_response(resp: lofi_common::Response) {
    match resp {
        lofi_common::Response::Ok => println!("ok"),
        lofi_common::Response::Error(msg) => eprintln!("error: {msg}"),
        lofi_common::Response::Status { mood, playing, current_source } => {
            let state = if playing { "playing" } else { "paused" };
            let source = current_source.unwrap_or_else(|| "none".to_string());
            println!("mood: {mood}\nstate: {state}\nsource: {source}");
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
        client::ensure_daemon_running(&socket)?;
        let resolved_mood = match mood {
            Some(m) => Some(m.clone()),
            None => client::classify_source(source)?,
        };
        if resolved_mood.is_none() {
            eprintln!(
                "could not classify '{source}' into a mood automatically; re-run with --mood <name>"
            );
            std::process::exit(1);
        }
        let resp = client::send_command(
            &socket,
            &DaemonCommand::Add { source: source.clone(), mood: resolved_mood },
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
        Cmd::Tui => unreachable!("handled above"),
        Cmd::Add { .. } => unreachable!("handled above"),
    };

    let resp = client::send_command(&socket, &cmd)?;
    print_response(resp);
    Ok(())
}

mod tui;
