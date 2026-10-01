use crate::client;
use crossterm::event::{self, Event, KeyCode};
use crossterm::terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen};
use crossterm::ExecutableCommand;
use ratatui::prelude::*;
use ratatui::widgets::{Block, Borders, List, ListItem, ListState, Padding, Paragraph};
use std::io::stdout;
use std::path::PathBuf;

pub fn run(socket: PathBuf) -> anyhow::Result<()> {
    let moods = match client::send_command(&socket, &lofi_common::Command::Moods)? {
        lofi_common::Response::Moods(names) => names,
        other => anyhow::bail!("unexpected response listing moods: {other:?}"),
    };

    let (mut current_mood, mut playing) =
        match client::send_command(&socket, &lofi_common::Command::Status) {
            Ok(lofi_common::Response::Status { mood, playing, .. }) => (mood, playing),
            _ => ("unknown".to_string(), true),
        };

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

            let state_label = if playing { "playing" } else { "paused" };
            let header = Paragraph::new(format!("mood: {current_mood}  ({state_label})"))
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

            let footer = Paragraph::new("enter: select  p: pause  r: resume  n: next  q: quit")
                .block(Block::default().borders(Borders::ALL).title("keys").padding(Padding::horizontal(1)));
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
                        if selected > 0 {
                            selected -= 1;
                        }
                    }
                    KeyCode::Down => {
                        if selected + 1 < moods.len() {
                            selected += 1;
                        }
                    }
                    KeyCode::Enter => {
                        let name = moods[selected].clone();
                        if client::send_command(&socket, &lofi_common::Command::Mood(name.clone())).is_ok() {
                            current_mood = name;
                            playing = true;
                        }
                    }
                    KeyCode::Char('p') => {
                        if client::send_command(&socket, &lofi_common::Command::Pause).is_ok() {
                            playing = false;
                        }
                    }
                    KeyCode::Char('r') => {
                        if client::send_command(&socket, &lofi_common::Command::Resume).is_ok() {
                            playing = true;
                        }
                    }
                    KeyCode::Char('n') => {
                        let _ = client::send_command(&socket, &lofi_common::Command::Next);
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
