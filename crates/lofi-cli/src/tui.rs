use crate::client;
use crossterm::event::{self, Event, KeyCode};
use crossterm::terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen};
use crossterm::ExecutableCommand;
use ratatui::prelude::*;
use ratatui::widgets::{Block, Borders, List, ListItem, ListState, Padding, Paragraph};
use std::io::stdout;
use std::path::{Path, PathBuf};

#[derive(Default)]
struct DaemonView {
    mood: String,
    state_label: &'static str,
    loop_playback: bool,
    audio_quality: String,
}

// The daemon is the source of truth for playback state (e.g. a mood with no
// sources never starts), so re-ask it rather than guessing from the command sent.
fn refresh_status(socket: &Path, view: &mut DaemonView) {
    if let Ok(lofi_common::Response::Status { mood, playing, paused, loop_playback, audio_quality, .. }) =
        client::send_command(socket, &lofi_common::Command::Status)
    {
        view.mood = mood;
        view.state_label = crate::playback_state_label(playing, paused);
        view.loop_playback = loop_playback;
        view.audio_quality = audio_quality;
    }
}

fn action_error(result: anyhow::Result<lofi_common::Response>) -> Option<String> {
    match result {
        Ok(lofi_common::Response::Error(msg)) => Some(msg),
        Ok(_) => None,
        Err(e) => Some(e.to_string()),
    }
}

pub fn run(socket: PathBuf) -> anyhow::Result<()> {
    let moods = match client::send_command(&socket, &lofi_common::Command::Moods)? {
        lofi_common::Response::Moods(names) => names,
        other => anyhow::bail!("unexpected response listing moods: {other:?}"),
    };

    let mut view = DaemonView { mood: "unknown".to_string(), state_label: "unknown", ..Default::default() };
    refresh_status(&socket, &mut view);
    // Cleared by the next successful action rather than on a timer.
    let mut last_error: Option<String> = None;

    enable_raw_mode()?;
    stdout().execute(EnterAlternateScreen)?;
    let backend = CrosstermBackend::new(stdout());
    let mut terminal = Terminal::new(backend)?;

    let mut selected = 0usize;
    let result = loop {
        if let Err(e) = terminal.draw(|frame| {
            let chunks = Layout::default()
                .direction(Direction::Vertical)
                .margin(1)
                .constraints([
                    Constraint::Length(3),
                    Constraint::Min(3),
                    Constraint::Length(3),
                ])
                .split(frame.area());

            let header = Paragraph::new(format!(
                "mood: {}  ({})  loop: {}  quality: {}",
                view.mood,
                view.state_label,
                crate::on_off_label(view.loop_playback),
                crate::quality_label(&view.audio_quality)
            ))
                .block(Block::default().borders(Borders::ALL).title("lofi").padding(Padding::horizontal(1)));
            frame.render_widget(header, chunks[0]);

            let items: Vec<ListItem> = moods.iter().map(|m| ListItem::new(m.as_str())).collect();
            let mut list_state = ListState::default();
            list_state.select(Some(selected));
            // Attribute-based highlight (not a color) so selection stays readable under any terminal theme.
            let list = List::new(items)
                .block(Block::default().borders(Borders::ALL).title("moods").padding(Padding::horizontal(1)))
                .highlight_style(Style::default().add_modifier(Modifier::REVERSED))
                .highlight_symbol(">> ");
            frame.render_stateful_widget(list, chunks[1], &mut list_state);

            let footer = match &last_error {
                Some(msg) => Paragraph::new(format!("error: {msg}"))
                    .style(Style::default().add_modifier(Modifier::BOLD))
                    .block(Block::default().borders(Borders::ALL).title("error").padding(Padding::horizontal(1))),
                None => Paragraph::new("enter: select  p: pause  r: resume  n: next  l: loop  a: audio quality  q: quit")
                    .block(Block::default().borders(Borders::ALL).title("keys").padding(Padding::horizontal(1))),
            };
            frame.render_widget(footer, chunks[2]);
        }) {
            break Err(e.into());
        }

        let has_event = match event::poll(std::time::Duration::from_millis(200)) {
            Ok(v) => v,
            Err(e) => break Err(e.into()),
        };
        if has_event {
            let event = match event::read() {
                Ok(e) => e,
                Err(e) => break Err(e.into()),
            };
            if let Event::Key(key) = event {
                match key.code {
                    KeyCode::Up => {
                        selected = selected.saturating_sub(1);
                    }
                    KeyCode::Down => {
                        if selected + 1 < moods.len() {
                            selected += 1;
                        }
                    }
                    KeyCode::Enter => {
                        let name = moods[selected].clone();
                        last_error = action_error(client::send_command(&socket, &lofi_common::Command::Mood(name)));
                        refresh_status(&socket, &mut view);
                    }
                    KeyCode::Char('p') => {
                        last_error = action_error(client::send_command(&socket, &lofi_common::Command::Pause));
                        refresh_status(&socket, &mut view);
                    }
                    KeyCode::Char('r') => {
                        last_error = action_error(client::send_command(&socket, &lofi_common::Command::Resume));
                        refresh_status(&socket, &mut view);
                    }
                    KeyCode::Char('n') => {
                        last_error = action_error(client::send_command(&socket, &lofi_common::Command::Next));
                        refresh_status(&socket, &mut view);
                    }
                    KeyCode::Char('l') => {
                        let toggled = lofi_common::Command::SetLoop(!view.loop_playback);
                        last_error = action_error(client::send_command(&socket, &toggled));
                        refresh_status(&socket, &mut view);
                    }
                    // q is quit, so audio quality gets a.
                    KeyCode::Char('a') => {
                        let next = if view.audio_quality == "max" { "min" } else { "max" };
                        let toggled = lofi_common::Command::SetAudioQuality(next.to_string());
                        last_error = action_error(client::send_command(&socket, &toggled));
                        refresh_status(&socket, &mut view);
                    }
                    KeyCode::Char('q') => break Ok(()),
                    _ => {}
                }
            }
        }
    };

    disable_raw_mode()?;
    stdout().execute(LeaveAlternateScreen)?;
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn action_error_surfaces_daemon_errors_and_transport_failures() {
        assert_eq!(
            action_error(Ok(lofi_common::Response::Error("mood 'ambient' has no sources configured".into()))),
            Some("mood 'ambient' has no sources configured".to_string())
        );
        assert_eq!(
            action_error(Err(anyhow::anyhow!("connection refused"))),
            Some("connection refused".to_string())
        );
        assert_eq!(action_error(Ok(lofi_common::Response::Ok)), None);
    }
}
