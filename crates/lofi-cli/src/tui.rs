use crate::client;
use crossterm::event::{self, Event, KeyCode};
use crossterm::terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen};
use crossterm::ExecutableCommand;
use ratatui::prelude::*;
use ratatui::widgets::{Block, Borders, List, ListItem, ListState};
use std::io::stdout;
use std::path::PathBuf;

pub fn run(socket: PathBuf) -> anyhow::Result<()> {
    let moods = match client::send_command(&socket, &lofi_common::Command::Moods)? {
        lofi_common::Response::Moods(names) => names,
        other => anyhow::bail!("unexpected response listing moods: {other:?}"),
    };

    enable_raw_mode()?;
    stdout().execute(EnterAlternateScreen)?;
    let backend = CrosstermBackend::new(stdout());
    let mut terminal = Terminal::new(backend)?;

    let mut selected = 0usize;
    let result = loop {
        terminal.draw(|frame| {
            let items: Vec<ListItem> = moods.iter().map(|m| ListItem::new(m.as_str())).collect();
            let mut state = ListState::default();
            state.select(Some(selected));
            let list = List::new(items)
                .block(Block::default().borders(Borders::ALL).title("lofi mood (enter: select, p: pause, r: resume, n: next, q: quit)"))
                .highlight_symbol(">> ");
            frame.render_stateful_widget(list, frame.area(), &mut state);
        })?;

        if event::poll(std::time::Duration::from_millis(200))? {
            if let Event::Key(key) = event::read()? {
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
                        let _ = client::send_command(&socket, &lofi_common::Command::Mood(name));
                    }
                    KeyCode::Char('p') => {
                        let _ = client::send_command(&socket, &lofi_common::Command::Pause);
                    }
                    KeyCode::Char('r') => {
                        let _ = client::send_command(&socket, &lofi_common::Command::Resume);
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
