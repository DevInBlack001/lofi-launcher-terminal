use crate::client::{self, PlaylistEntry, Probe, VideoInfo};
use lofi_common::{Command, Config, Response};
use std::collections::{BTreeMap, HashSet};
use std::path::Path;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum SkipReason {
    Unavailable,
    NestedPlaylist,
    NoUrl,
    Unclassifiable,
    AlreadyAdded,
}

impl SkipReason {
    fn label(self) -> &'static str {
        match self {
            SkipReason::Unavailable => "skipped (private or deleted)",
            SkipReason::NestedPlaylist => "skipped (nested playlist)",
            SkipReason::NoUrl => "skipped (no usable URL)",
            SkipReason::Unclassifiable => "skipped (unclassifiable)",
            SkipReason::AlreadyAdded => "skipped (already added)",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EntryPlan {
    Add { url: String, mood: String, is_live: bool },
    Skip(SkipReason),
}

// Each entry is classified on its own title, like a single `lofi add <url>`.
// An explicit --mood is only the fallback for entries whose title matches no
// mood, so one stray keyword-less title does not drag the whole playlist
// into one mood, and an unmatched entry is still added somewhere the user chose.
pub fn plan_playlist_entry(entry: &PlaylistEntry, config: &Config, fallback_mood: Option<&str>) -> EntryPlan {
    if entry.nested_playlist {
        return EntryPlan::Skip(SkipReason::NestedPlaylist);
    }
    let Some(url) = entry.url.clone() else {
        return EntryPlan::Skip(SkipReason::NoUrl);
    };
    let Some(title) = entry.title.as_deref() else {
        return EntryPlan::Skip(SkipReason::Unavailable);
    };
    let classified = lofi_common::classify(&config.classifier, title, &entry.description)
        .filter(|mood| config.moods.contains_key(mood));
    match classified.or_else(|| fallback_mood.map(str::to_string)) {
        Some(mood) => EntryPlan::Add { url, mood, is_live: entry.is_live },
        None => EntryPlan::Skip(SkipReason::Unclassifiable),
    }
}

#[derive(Debug, Default, PartialEq, Eq)]
pub struct Tally {
    pub added: BTreeMap<String, usize>,
    pub skipped: BTreeMap<SkipReason, usize>,
    pub failed: usize,
}

impl Tally {
    fn added_total(&self) -> usize {
        self.added.values().sum()
    }

    pub fn summary(&self, total: usize) -> String {
        let mut parts: Vec<String> = self.added.iter().map(|(mood, n)| format!("{mood}: {n}")).collect();
        parts.extend(self.skipped.iter().map(|(reason, n)| format!("{}: {n}", reason.label())));
        if self.failed > 0 {
            parts.push(format!("failed: {}", self.failed));
        }
        let mut line = format!("Added {} of {total} entries", self.added_total());
        if !parts.is_empty() {
            line.push_str(" (");
            line.push_str(&parts.join(", "));
            line.push(')');
        }
        line
    }
}

fn load_config() -> anyhow::Result<Config> {
    // Package installs never seed config.toml; the daemon falls back the same way.
    lofi_common::load_config_or_default(&lofi_common::config_path())
}

// Returns the process exit code.
pub fn run(socket: &Path, source: &str, mood: Option<&str>) -> anyhow::Result<i32> {
    let source = client::resolve_source(source)?;
    // Checked before anything that could run yt-dlp, so a local path never
    // reaches the playlist probe or a download.
    if !client::is_local_source(&source) {
        return add_url(socket, &source, mood);
    }
    let resolved_mood = match mood {
        Some(m) => Some(m.to_string()),
        None => client::classify_source(&source)?,
    };
    if resolved_mood.is_none() {
        eprintln!("could not classify '{source}' into a mood automatically; re-run with --mood <name>");
        return Ok(1);
    }
    let resp = client::send_command(socket, &Command::Add { source, mood: resolved_mood })?;
    crate::print_response(resp);
    Ok(0)
}

// A URL source: probe for a playlist first, then add each resulting source
// through the daemon's ordinary one-source Add.
fn add_url(socket: &Path, source: &str, mood: Option<&str>) -> anyhow::Result<i32> {
    match client::probe_url(source) {
        Probe::Playlist { title, entries, truncated_from } => {
            add_playlist(socket, &title, &entries, truncated_from, mood)
        }
        Probe::Single(info) => add_single(socket, source, Some(info), mood),
        Probe::Unknown => add_single(socket, source, client::fetch_video_info(source), mood),
    }
}

fn add_single(socket: &Path, source: &str, info: Option<VideoInfo>, mood: Option<&str>) -> anyhow::Result<i32> {
    let resolved_mood = match mood {
        Some(m) => Some(m.to_string()),
        None => match &info {
            Some(info) => lofi_common::classify(&load_config()?.classifier, &info.title, &info.description),
            None => None,
        },
    };
    let Some(resolved_mood) = resolved_mood else {
        eprintln!("could not classify '{source}' into a mood automatically; re-run with --mood <name>");
        return Ok(1);
    };
    let resp = client::send_command(
        socket,
        &Command::Add { source: source.to_string(), mood: Some(resolved_mood) },
    )?;
    crate::print_response(resp);
    Ok(0)
}

fn display_title(entry: &PlaylistEntry) -> String {
    entry.title.as_deref().map(client::sanitize_for_display).unwrap_or_else(|| "(untitled)".to_string())
}

fn add_playlist(
    socket: &Path,
    title: &str,
    entries: &[PlaylistEntry],
    truncated_from: Option<usize>,
    fallback_mood: Option<&str>,
) -> anyhow::Result<i32> {
    let config = load_config()?;
    if let Some(m) = fallback_mood {
        if !config.moods.contains_key(m) {
            let valid = config.moods.keys().cloned().collect::<Vec<_>>().join(", ");
            eprintln!("unknown mood '{m}', valid moods: {valid}");
            return Ok(1);
        }
    }
    let total = entries.len();
    let title = client::sanitize_for_display(title);
    println!("playlist '{}': {total} entries", if title.is_empty() { "untitled" } else { &title });
    if let Some(full) = truncated_from {
        eprintln!("warning: the playlist has {full} entries; only the first {total} are considered");
    }

    let mut known: HashSet<String> =
        config.moods.values().flat_map(|m| m.sources.iter().cloned()).collect();
    let mut tally = Tally::default();
    for entry in entries {
        let label = display_title(entry);
        let (url, mood) = match plan_playlist_entry(entry, &config, fallback_mood) {
            EntryPlan::Add { url, mood, .. } => (url, mood),
            EntryPlan::Skip(reason) => {
                if reason == SkipReason::Unclassifiable {
                    if let Some(url) = &entry.url {
                        eprintln!("no mood matched '{label}'; add it with: lofi add '{url}' --mood <name>");
                    }
                }
                *tally.skipped.entry(reason).or_default() += 1;
                continue;
            }
        };
        if !known.insert(url.clone()) {
            *tally.skipped.entry(SkipReason::AlreadyAdded).or_default() += 1;
            continue;
        }
        add_entry_source(socket, entry.position, total, &label, url, mood, &mut tally);
    }
    println!("{}", tally.summary(total));
    let had_work = tally.added_total() + tally.failed > 0;
    Ok(if had_work && tally.added_total() == 0 { 1 } else { 0 })
}

fn add_entry_source(
    socket: &Path,
    position: usize,
    total: usize,
    label: &str,
    source: String,
    mood: String,
    tally: &mut Tally,
) {
    match client::send_command(socket, &Command::Add { source, mood: Some(mood.clone()) }) {
        Ok(Response::Error(msg)) => {
            eprintln!("warning: could not add entry {position} '{label}': {msg}");
            tally.failed += 1;
        }
        Err(e) => {
            eprintln!("warning: could not add entry {position} '{label}': {e}");
            tally.failed += 1;
        }
        Ok(_) => {
            println!("[{position}/{total}] {mood}: {label}");
            *tally.added.entry(mood).or_default() += 1;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn default_config() -> Config {
        lofi_common::load_config_or_default(Path::new("/nonexistent/lofi-test/config.toml")).unwrap()
    }

    fn entry(title: Option<&str>) -> PlaylistEntry {
        PlaylistEntry {
            position: 1,
            url: Some("https://www.youtube.com/watch?v=abcdefghijk".to_string()),
            title: title.map(str::to_string),
            description: String::new(),
            is_live: false,
            nested_playlist: false,
        }
    }

    #[test]
    fn each_playlist_entry_is_classified_on_its_own_title() {
        let config = default_config();
        let plan = |t| plan_playlist_entry(&entry(Some(t)), &config, None);
        assert!(matches!(plan("Heavy rain and thunder"), EntryPlan::Add { mood, .. } if mood == "rainy-day"));
        assert!(matches!(plan("Deep Focus Music"), EntryPlan::Add { mood, .. } if mood == "deep-focus"));
        assert_eq!(plan("Untitled 7"), EntryPlan::Skip(SkipReason::Unclassifiable));
    }

    #[test]
    fn explicit_mood_is_only_the_fallback_for_unmatched_entries() {
        let config = default_config();
        let plan = |t| plan_playlist_entry(&entry(Some(t)), &config, Some("ambient"));
        assert!(matches!(plan("Heavy rain"), EntryPlan::Add { mood, .. } if mood == "rainy-day"));
        assert!(matches!(plan("Untitled 7"), EntryPlan::Add { mood, .. } if mood == "ambient"));
    }

    #[test]
    fn a_classifier_mood_missing_from_moods_falls_through_to_the_fallback() {
        let mut config = default_config();
        config.moods.remove("rainy-day");
        assert_eq!(
            plan_playlist_entry(&entry(Some("Heavy rain")), &config, None),
            EntryPlan::Skip(SkipReason::Unclassifiable)
        );
        assert!(matches!(
            plan_playlist_entry(&entry(Some("Heavy rain")), &config, Some("ambient")),
            EntryPlan::Add { mood, .. } if mood == "ambient"
        ));
    }

    #[test]
    fn unusable_entries_are_skipped_even_with_a_fallback_mood() {
        let config = default_config();
        assert_eq!(
            plan_playlist_entry(&entry(None), &config, Some("ambient")),
            EntryPlan::Skip(SkipReason::Unavailable)
        );
        let mut nested = entry(Some("rain"));
        nested.nested_playlist = true;
        assert_eq!(plan_playlist_entry(&nested, &config, Some("ambient")), EntryPlan::Skip(SkipReason::NestedPlaylist));
        let mut no_url = entry(Some("rain"));
        no_url.url = None;
        assert_eq!(plan_playlist_entry(&no_url, &config, Some("ambient")), EntryPlan::Skip(SkipReason::NoUrl));
    }

    use crate::client::test_support::{env_lock, fake_yt_dlp_script, recorded_yt_dlp_args};
    use std::io::{BufRead, BufReader, Write};
    use std::sync::{Arc, Mutex};

    // Answers every connection like the real daemon would, recording what it
    // was sent. Adds to the mood "broken" fail, to exercise the warn-and-continue path.
    fn fake_daemon(socket: &Path) -> Arc<Mutex<Vec<Command>>> {
        let listener = std::os::unix::net::UnixListener::bind(socket).unwrap();
        let received = Arc::new(Mutex::new(Vec::new()));
        let log = received.clone();
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                let stream = stream.unwrap();
                let mut writer = stream.try_clone().unwrap();
                let mut line = String::new();
                BufReader::new(stream).read_line(&mut line).unwrap();
                let cmd = lofi_common::decode_command(line.trim_end()).unwrap();
                let resp = match &cmd {
                    Command::Add { mood: Some(m), .. } if m == "broken" => Response::Error("boom".to_string()),
                    Command::Add { mood: Some(m), .. } => Response::Classified(m.clone()),
                    Command::Status => Response::Status {
                        mood: "ambient".to_string(),
                        playing: false,
                        paused: false,
                        current_source: None,
                        loop_playback: true,
                        audio_quality: "max".to_string(),
                    },
                    _ => Response::Ok,
                };
                log.lock().unwrap().push(cmd);
                writer.write_all(lofi_common::encode_response(&resp).as_bytes()).unwrap();
            }
        });
        received
    }

    fn adds(received: &Arc<Mutex<Vec<Command>>>) -> Vec<(String, String)> {
        received
            .lock()
            .unwrap()
            .iter()
            .filter_map(|c| match c {
                Command::Add { source, mood } => Some((source.clone(), mood.clone().unwrap())),
                _ => None,
            })
            .collect()
    }

    const PLAYLIST_JSON: &str = r#"{"_type": "playlist", "title": "mixed", "entries": [
        {"_type": "url", "ie_key": "Youtube", "id": "aaaaaaaaaaa", "url": "https://www.youtube.com/watch?v=aaaaaaaaaaa", "title": "Heavy rain for sleeping"},
        {"_type": "url", "ie_key": "Youtube", "id": "bbbbbbbbbbb", "url": "https://www.youtube.com/watch?v=bbbbbbbbbbb", "title": "Deep focus music"},
        {"_type": "url", "ie_key": "Youtube", "id": "ccccccccccc", "url": "https://www.youtube.com/watch?v=ccccccccccc", "title": "Untitled track 7"},
        {"_type": "url", "ie_key": "Youtube", "id": "ddddddddddd", "url": "https://www.youtube.com/watch?v=ddddddddddd", "title": null}
    ]}"#;

    struct Env {
        dir: tempfile::TempDir,
        socket: std::path::PathBuf,
        received: Arc<Mutex<Vec<Command>>>,
    }

    fn setup(yt_dlp_stdout: &str) -> Env {
        let dir = tempfile::tempdir().unwrap();
        let yt_dlp = fake_yt_dlp_script(dir.path(), yt_dlp_stdout);
        std::env::set_var("LOFI_YTDLP_BIN", &yt_dlp);
        std::env::set_var("XDG_CONFIG_HOME", dir.path().join("config-home"));
        std::env::set_var("XDG_DATA_HOME", dir.path().join("data-home"));
        let socket = dir.path().join("daemon.sock");
        let received = fake_daemon(&socket);
        Env { dir, socket, received }
    }

    fn teardown() {
        for var in ["LOFI_YTDLP_BIN", "XDG_CONFIG_HOME", "XDG_DATA_HOME"] {
            std::env::remove_var(var);
        }
    }

    #[test]
    fn a_streamed_playlist_adds_each_entry_to_its_own_mood() {
        let _guard = env_lock();
        let env = setup(PLAYLIST_JSON);
        let code = run(&env.socket, "https://www.youtube.com/playlist?list=PLx", None);
        teardown();
        assert_eq!(code.unwrap(), 0);
        assert_eq!(
            adds(&env.received),
            vec![
                ("https://www.youtube.com/watch?v=aaaaaaaaaaa".to_string(), "rainy-day".to_string()),
                ("https://www.youtube.com/watch?v=bbbbbbbbbbb".to_string(), "deep-focus".to_string()),
            ]
        );
    }

    #[test]
    fn a_streamed_playlist_uses_the_explicit_mood_only_for_unmatched_entries_and_survives_failures() {
        let _guard = env_lock();
        let env = setup(PLAYLIST_JSON);
        let config_dir = env.dir.path().join("config-home/lofi-launcher");
        std::fs::create_dir_all(&config_dir).unwrap();
        let config = lofi_common::default_config_toml().to_string() + "\n[moods.broken]\nsources = []\n";
        std::fs::write(config_dir.join("config.toml"), config).unwrap();
        let code = run(&env.socket, "https://www.youtube.com/playlist?list=PLx", Some("broken"));
        teardown();
        assert_eq!(code.unwrap(), 0);
        let added = adds(&env.received);
        assert_eq!(added.len(), 3, "{added:?}");
        assert_eq!(added[2], ("https://www.youtube.com/watch?v=ccccccccccc".to_string(), "broken".to_string()));
    }

    #[test]
    fn a_playlist_with_an_unknown_fallback_mood_adds_nothing() {
        let _guard = env_lock();
        let env = setup(PLAYLIST_JSON);
        let code = run(&env.socket, "https://www.youtube.com/playlist?list=PLx", Some("nope"));
        teardown();
        assert_eq!(code.unwrap(), 1);
        assert!(adds(&env.received).is_empty());
    }

    #[test]
    fn a_single_video_url_is_added_as_the_url_itself() {
        let _guard = env_lock();
        let env = setup(r#"{"_type": "video", "title": "Heavy rain and thunder", "description": ""}"#);
        let code = run(&env.socket, "https://www.youtube.com/watch?v=aaaaaaaaaaa", None);
        teardown();
        assert_eq!(code.unwrap(), 0);
        assert_eq!(
            adds(&env.received),
            vec![("https://www.youtube.com/watch?v=aaaaaaaaaaa".to_string(), "rainy-day".to_string())]
        );
        let args = recorded_yt_dlp_args(env.dir.path());
        assert!(args.contains(&"--flat-playlist".to_string()), "the probe alone should have sufficed: {args:?}");
    }

    #[test]
    fn a_url_whose_metadata_cannot_be_read_still_works_with_an_explicit_mood() {
        let _guard = env_lock();
        let env = setup("");
        std::env::set_var("LOFI_YTDLP_BIN", "/nonexistent/definitely-not-yt-dlp");
        let unclassified = run(&env.socket, "https://example.com/a", None);
        let explicit = run(&env.socket, "https://example.com/a", Some("ambient"));
        teardown();
        assert_eq!(unclassified.unwrap(), 1);
        assert_eq!(explicit.unwrap(), 0);
        assert_eq!(adds(&env.received), vec![("https://example.com/a".to_string(), "ambient".to_string())]);
    }

    #[test]
    fn a_local_path_never_runs_yt_dlp_and_is_added_as_its_absolute_path() {
        let _guard = env_lock();
        let env = setup(PLAYLIST_JSON);
        let local = env.dir.path().join("mix.flac");
        std::fs::write(&local, b"").unwrap();
        let canonical = std::fs::canonicalize(&local).unwrap().to_str().unwrap().to_string();
        let with_mood = run(&env.socket, local.to_str().unwrap(), Some("ambient"));
        let without_mood = run(&env.socket, local.to_str().unwrap(), None);
        teardown();
        assert_eq!(with_mood.unwrap(), 0);
        assert_eq!(without_mood.unwrap(), 1, "a local path still needs --mood");
        assert_eq!(adds(&env.received), vec![(canonical, "ambient".to_string())]);
        assert!(!env.dir.path().join("yt-dlp-args").exists(), "yt-dlp was invoked for a local source");
    }

    #[test]
    fn summary_lists_per_mood_counts_then_skips_then_failures() {
        let mut tally = Tally::default();
        tally.added.insert("code-and-chill".to_string(), 4);
        tally.added.insert("ambient".to_string(), 2);
        tally.skipped.insert(SkipReason::Unclassifiable, 1);
        assert_eq!(
            tally.summary(7),
            "Added 6 of 7 entries (ambient: 2, code-and-chill: 4, skipped (unclassifiable): 1)"
        );
        tally.failed = 1;
        assert!(tally.summary(8).ends_with(", failed: 1)"));
        assert_eq!(Tally::default().summary(0), "Added 0 of 0 entries");
    }
}
