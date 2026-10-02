use crate::client::{self, Chapter};
use crossterm::event::{self, Event, KeyCode};
use crossterm::terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen};
use crossterm::ExecutableCommand;
use ratatui::prelude::*;
use ratatui::widgets::{Block, Borders, List, ListItem, ListState, Padding, Paragraph};
use std::collections::{HashMap, HashSet};
use std::io::stdout;
use std::path::{Path, PathBuf};
use std::sync::mpsc;

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

fn format_timestamp(seconds: u64) -> String {
    format!("{:02}:{:02}:{:02}", seconds / 3600, (seconds / 60) % 60, seconds % 60)
}

const MAX_BREADCRUMB_SOURCE_CHARS: usize = 48;

fn short_source_label(source: &str) -> String {
    let label = if client::is_local_source(source) {
        Path::new(source)
            .file_name()
            .map(|name| name.to_string_lossy().to_string())
            .unwrap_or_else(|| source.to_string())
    } else {
        source.to_string()
    };
    let label = client::sanitize_for_display(&label);
    if label.chars().count() > MAX_BREADCRUMB_SOURCE_CHARS {
        let kept: String = label.chars().take(MAX_BREADCRUMB_SOURCE_CHARS - 3).collect();
        format!("{kept}...")
    } else {
        label
    }
}

enum Level {
    Moods,
    Sources { mood: String, sources: Vec<String> },
    Chapters { mood: String, sources: Vec<String>, index: usize, chapters: Vec<Chapter> },
}

// What the I/O loop must do after a key press; the browser itself never
// talks to the daemon or spawns yt-dlp, which keeps its navigation testable.
#[derive(Debug, PartialEq, Eq)]
enum Action {
    None,
    ListSources(String),
    FetchChapters(String),
    Play { mood: String, index: usize, seek_seconds: Option<u64> },
    SwitchMood(String),
    RemoveSource { mood: String, source: String },
}

struct Browser {
    moods: Vec<String>,
    level: Level,
    mood_selected: usize,
    source_selected: usize,
    chapter_selected: usize,
    chapter_cache: HashMap<String, Vec<Chapter>>,
    loading: HashSet<String>,
    // The URL source Enter was pressed on while its chapters were still
    // loading; acted on when they arrive, unless the user has moved since.
    awaiting: Option<(String, usize, String)>,
    // Source waiting for delete confirmation in Sources view
    delete_pending: Option<String>,
}

impl Browser {
    fn new(moods: Vec<String>) -> Self {
        Self {
            moods,
            level: Level::Moods,
            mood_selected: 0,
            source_selected: 0,
            chapter_selected: 0,
            chapter_cache: HashMap::new(),
            loading: HashSet::new(),
            awaiting: None,
            delete_pending: None,
        }
    }

    fn len(&self) -> usize {
        match &self.level {
            Level::Moods => self.moods.len(),
            Level::Sources { sources, .. } => sources.len(),
            // Entry 0 plays the whole source from its start.
            Level::Chapters { chapters, .. } => chapters.len() + 1,
        }
    }

    fn selected(&self) -> usize {
        match self.level {
            Level::Moods => self.mood_selected,
            Level::Sources { .. } => self.source_selected,
            Level::Chapters { .. } => self.chapter_selected,
        }
    }

    fn set_selected(&mut self, value: usize) {
        self.awaiting = None;
        self.delete_pending = None;
        match self.level {
            Level::Moods => self.mood_selected = value,
            Level::Sources { .. } => self.source_selected = value,
            Level::Chapters { .. } => self.chapter_selected = value,
        }
    }

    fn up(&mut self) {
        self.set_selected(self.selected().saturating_sub(1));
    }

    fn down(&mut self) {
        if self.selected() + 1 < self.len() {
            self.set_selected(self.selected() + 1);
        }
    }

    fn back(&mut self) {
        self.awaiting = None;
        self.delete_pending = None;
        self.level = match std::mem::replace(&mut self.level, Level::Moods) {
            Level::Moods | Level::Sources { .. } => Level::Moods,
            Level::Chapters { mood, sources, .. } => Level::Sources { mood, sources },
        };
    }

    fn show_sources(&mut self, mood: String, sources: Vec<String>) {
        self.awaiting = None;
        self.delete_pending = None;
        self.source_selected = 0;
        self.level = Level::Sources { mood, sources };
    }

    fn show_chapters(&mut self, mood: String, sources: Vec<String>, index: usize, chapters: Vec<Chapter>) {
        self.chapter_selected = 0;
        self.level = Level::Chapters { mood, sources, index, chapters };
    }

    fn enter(&mut self) -> Action {
        match &self.level {
            Level::Moods => match self.moods.get(self.mood_selected) {
                Some(mood) => Action::ListSources(mood.clone()),
                None => Action::None,
            },
            Level::Sources { mood, sources } => {
                let index = self.source_selected;
                let Some(source) = sources.get(index).cloned() else { return Action::None };
                // Local files have no chapters to look up, and must never cost a yt-dlp call.
                if client::is_local_source(&source) {
                    return Action::Play { mood: mood.clone(), index, seek_seconds: None };
                }
                match self.chapter_cache.get(&source) {
                    Some(chapters) if !chapters.is_empty() => {
                        let (mood, sources, chapters) = (mood.clone(), sources.clone(), chapters.clone());
                        self.show_chapters(mood, sources, index, chapters);
                        Action::None
                    }
                    Some(_) => Action::Play { mood: mood.clone(), index, seek_seconds: None },
                    None => {
                        self.awaiting = Some((mood.clone(), index, source.clone()));
                        if self.loading.insert(source.clone()) {
                            Action::FetchChapters(source)
                        } else {
                            Action::None
                        }
                    }
                }
            }
            Level::Chapters { mood, index, chapters, .. } => {
                let seek_seconds = match self.chapter_selected {
                    0 => None,
                    n => chapters.get(n - 1).map(|c| c.start_seconds),
                };
                Action::Play { mood: mood.clone(), index: *index, seek_seconds }
            }
        }
    }

    // Space: act right away without drilling any deeper.
    fn play_from_start(&mut self) -> Action {
        match &self.level {
            Level::Moods => match self.moods.get(self.mood_selected) {
                Some(mood) => Action::SwitchMood(mood.clone()),
                None => Action::None,
            },
            Level::Sources { mood, sources } if self.source_selected < sources.len() => {
                Action::Play { mood: mood.clone(), index: self.source_selected, seek_seconds: None }
            }
            Level::Sources { .. } => Action::None,
            Level::Chapters { mood, index, .. } => Action::Play { mood: mood.clone(), index: *index, seek_seconds: None },
        }
    }

    // A failed fetch arrives as an empty list, so that source just plays from
    // its start rather than being retried on every Enter.
    fn chapters_arrived(&mut self, source: &str, chapters: Vec<Chapter>) -> Action {
        self.loading.remove(source);
        self.chapter_cache.insert(source.to_string(), chapters.clone());
        let Some((awaited_mood, awaited_index, awaited_source)) = self.awaiting.take() else {
            return Action::None;
        };
        let still_there = matches!(
            &self.level,
            Level::Sources { mood, .. } if *mood == awaited_mood && self.source_selected == awaited_index
        );
        if awaited_source != source || !still_there {
            self.awaiting = Some((awaited_mood, awaited_index, awaited_source));
            return Action::None;
        }
        if chapters.is_empty() {
            return Action::Play { mood: awaited_mood, index: awaited_index, seek_seconds: None };
        }
        if let Level::Sources { sources, .. } = &self.level {
            let sources = sources.clone();
            self.show_chapters(awaited_mood, sources, awaited_index, chapters);
        }
        Action::None
    }

    fn awaiting_source(&self) -> Option<&str> {
        self.awaiting.as_ref().map(|(_, _, source)| source.as_str())
    }

    // Only 'd' calls this. It only arms; it never confirms on its own, so a
    // bare Enter press can never be mistaken for arming a delete.
    fn arm_delete(&mut self) {
        if let Level::Sources { sources, .. } = &self.level {
            if let Some(source) = sources.get(self.source_selected) {
                self.delete_pending = Some(source.clone());
            }
        }
    }

    // Only Enter calls this. It never arms a delete itself, it only checks
    // whether the currently selected source already has one armed (via 'd')
    // and, if so, confirms it. This keeps a plain Enter press on an
    // unarmed source doing its normal job (play/show chapters).
    fn confirm_delete_if_pending(&mut self) -> Option<(String, String)> {
        match &self.level {
            Level::Sources { mood, sources } if self.source_selected < sources.len() => {
                let source = sources.get(self.source_selected)?;
                if self.delete_pending.as_ref() == Some(source) {
                    self.delete_pending = None;
                    return Some((mood.clone(), source.clone()));
                }
                None
            }
            _ => None,
        }
    }

    fn cancel_delete(&mut self) {
        self.delete_pending = None;
    }

    fn breadcrumb(&self) -> String {
        match &self.level {
            Level::Moods => "moods".to_string(),
            Level::Sources { mood, .. } => format!("moods > {mood}"),
            Level::Chapters { mood, sources, index, .. } => {
                let source = sources.get(*index).map(|s| short_source_label(s)).unwrap_or_default();
                format!("moods > {mood} > {source} > chapters")
            }
        }
    }

    fn list_title(&self) -> &'static str {
        match self.level {
            Level::Moods => "moods",
            Level::Sources { .. } => "sources",
            Level::Chapters { .. } => "chapters",
        }
    }

    fn items(&self) -> Vec<String> {
        match &self.level {
            Level::Moods => self.moods.iter().map(|m| client::sanitize_for_display(m)).collect(),
            Level::Sources { sources, .. } if sources.is_empty() => vec!["(no sources in this mood)".to_string()],
            Level::Sources { sources, .. } => sources
                .iter()
                .map(|source| {
                    if client::is_local_source(source) {
                        return format!("[file] {}", short_source_label(source));
                    }
                    let label = client::sanitize_for_display(source);
                    if self.loading.contains(source) {
                        format!("{label}  (loading chapters...)")
                    } else {
                        match self.chapter_cache.get(source) {
                            Some(chapters) if !chapters.is_empty() => format!("{label}  ({} chapters)", chapters.len()),
                            _ => label,
                        }
                    }
                })
                .collect(),
            Level::Chapters { chapters, .. } => std::iter::once("(whole source from the start)".to_string())
                .chain(chapters.iter().map(|c| format!("[{}] {}", format_timestamp(c.start_seconds), c.title)))
                .collect(),
        }
    }

    fn key_help(&self) -> &'static str {
        match self.level {
            Level::Moods => "enter: open  space: play mood  p/r: pause/resume  n: next  l: loop  a: quality  q: quit",
            Level::Sources { .. } => {
                "enter: play/chapters  space: play from start  d: delete  esc: back  p/r  n  l  a  q"
            }
            Level::Chapters { .. } => "enter: play from here  esc: back  p/r: pause/resume  n  l: loop  a: quality  q: quit",
        }
    }
}

type ChapterResult = (String, Result<Vec<Chapter>, String>);

fn spawn_chapter_fetch(source: String, results: mpsc::Sender<ChapterResult>) {
    std::thread::spawn(move || {
        let result = client::fetch_chapters(&source).map_err(|e| e.to_string());
        let _ = results.send((source, result));
    });
}

// Returns the error to show, if any.
fn perform(
    action: Action,
    socket: &Path,
    browser: &mut Browser,
    chapter_results: &mpsc::Sender<ChapterResult>,
) -> Option<String> {
    let command = match action {
        Action::None => return None,
        Action::FetchChapters(source) => {
            spawn_chapter_fetch(source, chapter_results.clone());
            return None;
        }
        Action::ListSources(mood) => {
            return match client::send_command(socket, &lofi_common::Command::Sources(mood.clone())) {
                Ok(lofi_common::Response::Sources(sources)) => {
                    browser.show_sources(mood, sources);
                    None
                }
                other => action_error(other).or_else(|| Some("unexpected response listing sources".to_string())),
            };
        }
        Action::Play { mood, index, seek_seconds } => lofi_common::Command::PlaySource { mood, index, seek_seconds },
        Action::SwitchMood(mood) => lofi_common::Command::Mood(mood),
        Action::RemoveSource { mood, source } => lofi_common::Command::RemoveSource { mood, source },
    };
    action_error(client::send_command(socket, &command))
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
    let mut notice: Option<String> = None;
    let mut browser = Browser::new(moods);
    let (chapter_results_tx, chapter_results) = mpsc::channel::<ChapterResult>();

    enable_raw_mode()?;
    stdout().execute(EnterAlternateScreen)?;
    let backend = CrosstermBackend::new(stdout());
    let mut terminal = Terminal::new(backend)?;

    let result = loop {
        while let Ok((source, fetched)) = chapter_results.try_recv() {
            let (chapters, fetch_error) = match fetched {
                Ok(chapters) => (chapters, None),
                Err(e) => (Vec::new(), Some(e)),
            };
            let action = browser.chapters_arrived(&source, chapters);
            if matches!(action, Action::Play { .. }) {
                last_error = perform(action, &socket, &mut browser, &chapter_results_tx);
                notice = fetch_error.map(|e| format!("no chapter list ({e}), playing from the start"));
                refresh_status(&socket, &mut view);
            }
        }

        if let Err(e) = terminal.draw(|frame| {
            let chunks = Layout::default()
                .direction(Direction::Vertical)
                .margin(1)
                .constraints([
                    Constraint::Length(4),
                    Constraint::Min(3),
                    Constraint::Length(3),
                ])
                .split(frame.area());

            let header = Paragraph::new(vec![
                Line::from(format!(
                    "mood: {}  ({})  loop: {}  quality: {}",
                    view.mood,
                    view.state_label,
                    crate::on_off_label(view.loop_playback),
                    crate::quality_label(&view.audio_quality)
                )),
                Line::from(format!("browsing: {}", browser.breadcrumb())),
            ])
            .block(Block::default().borders(Borders::ALL).title("lofi").padding(Padding::horizontal(1)));
            frame.render_widget(header, chunks[0]);

            let items: Vec<ListItem> = browser.items().into_iter().map(ListItem::new).collect();
            let mut list_state = ListState::default();
            list_state.select(Some(browser.selected()));
            // Attribute-based highlight (not a color) so selection stays readable under any terminal theme.
            let list = List::new(items)
                .block(Block::default().borders(Borders::ALL).title(browser.list_title()).padding(Padding::horizontal(1)))
                .highlight_style(Style::default().add_modifier(Modifier::REVERSED))
                .highlight_symbol(">> ");
            frame.render_stateful_widget(list, chunks[1], &mut list_state);

            let footer = if let Some(msg) = &last_error {
                Paragraph::new(format!("error: {msg}"))
                    .style(Style::default().add_modifier(Modifier::BOLD))
                    .block(Block::default().borders(Borders::ALL).title("error").padding(Padding::horizontal(1)))
            } else if let Some(source) = browser.awaiting_source() {
                Paragraph::new(format!("loading chapters for {}...", short_source_label(source)))
                    .block(Block::default().borders(Borders::ALL).title("loading").padding(Padding::horizontal(1)))
            } else if let Some(source) = &browser.delete_pending {
                Paragraph::new(format!("press enter to confirm delete of '{}'  esc/backspace to cancel", short_source_label(source)))
                    .style(Style::default().add_modifier(Modifier::BOLD))
                    .block(Block::default().borders(Borders::ALL).title("confirm delete").padding(Padding::horizontal(1)))
            } else if let Some(msg) = &notice {
                Paragraph::new(msg.as_str())
                    .block(Block::default().borders(Borders::ALL).title("note").padding(Padding::horizontal(1)))
            } else {
                Paragraph::new(browser.key_help())
                    .block(Block::default().borders(Borders::ALL).title("keys").padding(Padding::horizontal(1)))
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
                notice = None;
                let daemon_command = match key.code {
                    KeyCode::Up => {
                        browser.cancel_delete();
                        browser.up();
                        None
                    }
                    KeyCode::Down => {
                        browser.cancel_delete();
                        browser.down();
                        None
                    }
                    KeyCode::Esc | KeyCode::Backspace | KeyCode::Left => {
                        browser.cancel_delete();
                        browser.back();
                        None
                    }
                    KeyCode::Enter => {
                        if let Some((mood, source)) = browser.confirm_delete_if_pending() {
                            let action = Action::RemoveSource { mood, source };
                            last_error = perform(action, &socket, &mut browser, &chapter_results_tx);
                            refresh_status(&socket, &mut view);
                            None
                        } else {
                            browser.cancel_delete();
                            let action = browser.enter();
                            last_error = perform(action, &socket, &mut browser, &chapter_results_tx);
                            refresh_status(&socket, &mut view);
                            None
                        }
                    }
                    KeyCode::Char(' ') => {
                        browser.cancel_delete();
                        let action = browser.play_from_start();
                        last_error = perform(action, &socket, &mut browser, &chapter_results_tx);
                        refresh_status(&socket, &mut view);
                        None
                    }
                    KeyCode::Char('d') => {
                        browser.arm_delete();
                        None
                    }
                    KeyCode::Char('p') => Some(lofi_common::Command::Pause),
                    KeyCode::Char('r') => Some(lofi_common::Command::Resume),
                    KeyCode::Char('n') => Some(lofi_common::Command::Next),
                    KeyCode::Char('l') => Some(lofi_common::Command::SetLoop(!view.loop_playback)),
                    // q is quit, so audio quality gets a.
                    KeyCode::Char('a') => {
                        let next = if view.audio_quality == "max" { "min" } else { "max" };
                        Some(lofi_common::Command::SetAudioQuality(next.to_string()))
                    }
                    KeyCode::Char('q') => break Ok(()),
                    _ => None,
                };
                if let Some(command) = daemon_command {
                    last_error = action_error(client::send_command(&socket, &command));
                    refresh_status(&socket, &mut view);
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

    const LOCAL: &str = "/music/rain.flac";
    const URL: &str = "https://www.youtube.com/watch?v=mix";

    fn chapters() -> Vec<Chapter> {
        vec![
            Chapter { title: "intro".to_string(), start_seconds: 0 },
            Chapter { title: "second".to_string(), start_seconds: 754 },
        ]
    }

    fn browser_in_ambient() -> Browser {
        let mut browser = Browser::new(vec!["ambient".to_string(), "deep-focus".to_string()]);
        assert_eq!(browser.enter(), Action::ListSources("ambient".to_string()));
        browser.show_sources("ambient".to_string(), vec![LOCAL.to_string(), URL.to_string()]);
        browser
    }

    fn play(index: usize, seek_seconds: Option<u64>) -> Action {
        Action::Play { mood: "ambient".to_string(), index, seek_seconds }
    }

    #[test]
    fn enter_on_a_local_source_plays_it_without_fetching_chapters() {
        let mut browser = browser_in_ambient();
        assert_eq!(browser.enter(), play(0, None));
        assert!(browser.loading.is_empty());
        assert_eq!(browser.delete_pending, None);
    }

    #[test]
    fn enter_on_a_url_fetches_chapters_once_then_drills_in_when_they_arrive() {
        let mut browser = browser_in_ambient();
        browser.down();
        assert_eq!(browser.enter(), Action::FetchChapters(URL.to_string()));
        assert_eq!(browser.enter(), Action::None, "a second Enter while loading must not refetch");
        assert!(browser.items()[1].contains("loading chapters"));

        assert_eq!(browser.chapters_arrived(URL, chapters()), Action::None);
        assert_eq!(browser.breadcrumb(), "moods > ambient > https://www.youtube.com/watch?v=mix > chapters");
        assert_eq!(browser.items(), vec!["(whole source from the start)", "[00:00:00] intro", "[00:12:34] second"]);
        browser.down();
        browser.down();
        assert_eq!(browser.enter(), play(1, Some(754)));
        browser.up();
        browser.up();
        assert_eq!(browser.enter(), play(1, None));
    }

    #[test]
    fn a_url_without_chapters_plays_from_the_start_when_the_fetch_finishes() {
        let mut browser = browser_in_ambient();
        browser.down();
        browser.enter();
        assert_eq!(browser.chapters_arrived(URL, Vec::new()), play(1, None));
        assert_eq!(browser.enter(), play(1, None), "cached empty result plays directly, no refetch");
    }

    #[test]
    fn chapters_arriving_after_the_user_moved_away_are_cached_but_not_acted_on() {
        let mut browser = browser_in_ambient();
        browser.down();
        browser.enter();
        browser.up();
        assert_eq!(browser.chapters_arrived(URL, chapters()), Action::None);
        assert_eq!(browser.breadcrumb(), "moods > ambient");
        browser.down();
        assert_eq!(browser.enter(), Action::None);
        assert_eq!(browser.breadcrumb(), "moods > ambient > https://www.youtube.com/watch?v=mix > chapters");
    }

    #[test]
    fn back_walks_up_one_level_at_a_time_and_keeps_the_parent_selection() {
        let mut browser = Browser::new(vec!["ambient".to_string(), "deep-focus".to_string()]);
        browser.down();
        assert_eq!(browser.enter(), Action::ListSources("deep-focus".to_string()));
        browser.show_sources("deep-focus".to_string(), vec![LOCAL.to_string(), URL.to_string()]);
        browser.down();
        browser.enter();
        browser.chapters_arrived(URL, chapters());
        assert_eq!(browser.list_title(), "chapters");

        browser.back();
        assert_eq!(browser.breadcrumb(), "moods > deep-focus");
        assert_eq!(browser.selected(), 1);
        browser.back();
        assert_eq!(browser.breadcrumb(), "moods");
        assert_eq!(browser.selected(), 1);
        browser.back();
        assert_eq!(browser.breadcrumb(), "moods");
    }

    #[test]
    fn space_plays_without_drilling_down_and_switches_mood_from_the_mood_list() {
        let mut browser = Browser::new(vec!["ambient".to_string()]);
        assert_eq!(browser.play_from_start(), Action::SwitchMood("ambient".to_string()));
        let mut browser = browser_in_ambient();
        browser.down();
        assert_eq!(browser.play_from_start(), play(1, None));
        assert!(browser.loading.is_empty(), "space must not fetch chapters");
    }

    #[test]
    fn empty_lists_never_produce_an_action_or_move_the_selection() {
        let mut browser = Browser::new(Vec::new());
        browser.down();
        assert_eq!(browser.enter(), Action::None);
        browser.show_sources("ambient".to_string(), Vec::new());
        browser.down();
        assert_eq!(browser.selected(), 0);
        assert_eq!(browser.enter(), Action::None);
        assert_eq!(browser.play_from_start(), Action::None);
        assert_eq!(browser.items(), vec!["(no sources in this mood)"]);
    }

    #[test]
    fn local_sources_are_labelled_by_file_name_and_long_labels_are_shortened() {
        assert_eq!(short_source_label(LOCAL), "rain.flac");
        let long = format!("https://example.com/{}", "a".repeat(100));
        let label = short_source_label(&long);
        assert!(label.ends_with("...") && label.chars().count() == MAX_BREADCRUMB_SOURCE_CHARS);
        assert_eq!(format_timestamp(3 * 3600 + 25 * 60 + 7), "03:25:07");
    }

    #[test]
    fn delete_requires_d_then_enter_to_confirm() {
        let mut browser = browser_in_ambient();
        assert_eq!(browser.delete_pending, None);
        browser.arm_delete();
        assert_eq!(browser.delete_pending.as_deref(), Some(LOCAL));
        let result = browser.confirm_delete_if_pending();
        assert_eq!(result, Some(("ambient".to_string(), LOCAL.to_string())));
        assert_eq!(browser.delete_pending, None);
    }

    #[test]
    fn delete_cancels_on_navigation() {
        let mut browser = browser_in_ambient();
        browser.arm_delete();
        assert_eq!(browser.delete_pending.as_deref(), Some(LOCAL));
        browser.down();
        assert_eq!(browser.delete_pending, None);
    }

    #[test]
    fn plain_enter_never_arms_or_confirms_a_delete() {
        // The real bug this guards against: Enter must never call the
        // arming half of the delete flow, only 'd' may. A bare Enter on a
        // source that was never armed with 'd' must leave delete_pending
        // untouched and report nothing to confirm.
        let mut browser = browser_in_ambient();
        assert_eq!(browser.delete_pending, None);
        let result = browser.confirm_delete_if_pending();
        assert_eq!(result, None);
        assert_eq!(browser.delete_pending, None);
    }
}
