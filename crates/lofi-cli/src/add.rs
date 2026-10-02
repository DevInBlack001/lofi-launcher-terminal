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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DownloadFlag {
    Ask,
    Download,
    NoDownload,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DownloadDecision {
    Download,
    Stream,
    // Same as Stream, but the user should hear why they were not asked.
    StreamNonInteractive,
    Prompt,
}

pub fn download_decision(flag: DownloadFlag, stdin_is_terminal: bool) -> DownloadDecision {
    match flag {
        DownloadFlag::Download => DownloadDecision::Download,
        DownloadFlag::NoDownload => DownloadDecision::Stream,
        DownloadFlag::Ask if stdin_is_terminal => DownloadDecision::Prompt,
        DownloadFlag::Ask => DownloadDecision::StreamNonInteractive,
    }
}

// "[y/N]": anything but an explicit yes, including just Enter, means no.
pub fn is_yes(answer: &str) -> bool {
    matches!(answer.trim().to_ascii_lowercase().as_str(), "y" | "yes")
}

#[derive(Debug, Clone)]
pub struct AddOptions {
    pub mood: Option<String>,
    pub download: DownloadFlag,
    pub stdin_is_terminal: bool,
}

fn ask_download(opts: &AddOptions, question: &str) -> anyhow::Result<bool> {
    use std::io::{BufRead, Read, Write};
    match download_decision(opts.download, opts.stdin_is_terminal) {
        DownloadDecision::Download => Ok(true),
        DownloadDecision::Stream => Ok(false),
        DownloadDecision::StreamNonInteractive => {
            eprintln!("note: stdin is not a terminal, so streaming instead of asking; pass --download to download without prompting");
            Ok(false)
        }
        DownloadDecision::Prompt => {
            print!("{question} [y/N]: ");
            std::io::stdout().flush()?;
            let mut answer = String::new();
            std::io::stdin().lock().take(1024).read_line(&mut answer)?;
            Ok(is_yes(&answer))
        }
    }
}

// Returns the process exit code.
pub fn run(socket: &Path, source: &str, opts: &AddOptions) -> anyhow::Result<i32> {
    let source = client::resolve_source(source)?;
    // Checked before anything that could run yt-dlp, so a local path never
    // reaches the playlist probe or a download.
    if !client::is_local_source(&source) {
        return add_url(socket, &source, opts);
    }
    if opts.download != DownloadFlag::Ask {
        eprintln!("note: --download and --no-download only apply to URL sources; ignored for a local path");
    }
    let resolved_mood = match &opts.mood {
        Some(m) => Some(m.clone()),
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
fn add_url(socket: &Path, source: &str, opts: &AddOptions) -> anyhow::Result<i32> {
    match client::probe_url(source) {
        Probe::Playlist { title, entries, truncated_from } => {
            add_playlist(socket, &title, &entries, truncated_from, opts)
        }
        Probe::Single(info) => add_single(socket, source, Some(info), opts),
        Probe::Unknown => add_single(socket, source, client::fetch_video_info(source), opts),
    }
}

// `lofi quality` is a session override held by the daemon, so it wins over
// the file; the file is only consulted when the daemon cannot say.
fn download_format(socket: &Path) -> &'static str {
    let quality = match client::send_command(socket, &Command::Status) {
        Ok(Response::Status { audio_quality, .. }) if lofi_common::is_valid_audio_quality(&audio_quality) => {
            audio_quality
        }
        _ => load_config().map(|c| c.audio_quality).unwrap_or_default(),
    };
    lofi_common::ytdl_format_for(&quality)
}

fn prepare_mood_dir(mood: &str) -> anyhow::Result<std::path::PathBuf> {
    let dir = lofi_common::mood_download_dir(mood)?;
    std::fs::create_dir_all(&dir).map_err(|e| anyhow::anyhow!("could not create {}: {e}", dir.display()))?;
    Ok(dir)
}

fn unknown_mood_message(config: &Config, mood: &str) -> String {
    let valid = config.moods.keys().cloned().collect::<Vec<_>>().join(", ");
    format!("unknown mood '{mood}', valid moods: {valid}")
}

fn add_single(socket: &Path, source: &str, info: Option<VideoInfo>, opts: &AddOptions) -> anyhow::Result<i32> {
    let resolved_mood = match &opts.mood {
        Some(m) => Some(m.clone()),
        None => match &info {
            Some(info) => lofi_common::classify(&load_config()?.classifier, &info.title, &info.description),
            None => None,
        },
    };
    let Some(resolved_mood) = resolved_mood else {
        eprintln!("could not classify '{source}' into a mood automatically; re-run with --mood <name>");
        return Ok(1);
    };

    let wants_download = match &info {
        None => {
            if opts.download == DownloadFlag::Download {
                eprintln!("cannot download '{source}': yt-dlp could not read it");
                return Ok(1);
            }
            false
        }
        Some(info) if info.is_live => {
            if opts.download == DownloadFlag::Download {
                eprintln!("note: '{source}' is a live stream, which cannot be downloaded; adding it as a streaming source");
            }
            false
        }
        Some(_) => ask_download(opts, "Download this locally instead of streaming?")?,
    };

    let final_source = if wants_download {
        let config = load_config()?;
        if !config.moods.contains_key(&resolved_mood) {
            eprintln!("{}", unknown_mood_message(&config, &resolved_mood));
            return Ok(1);
        }
        let dir = prepare_mood_dir(&resolved_mood)?;
        let outcome = client::download_audio(source, &dir, SINGLE_FILENAME_TEMPLATE, download_format(socket))?;
        let Some(path) = outcome.saved.into_iter().next() else {
            eprintln!("download of '{source}' failed; nothing was added");
            return Ok(1);
        };
        if !outcome.exited_ok {
            eprintln!("warning: yt-dlp reported an error after saving {path}; adding the saved file anyway");
        }
        println!("saved to {path}");
        path
    } else {
        source.to_string()
    };

    let resp = client::send_command(socket, &Command::Add { source: final_source, mood: Some(resolved_mood) })?;
    crate::print_response(resp);
    Ok(0)
}

fn display_title(entry: &PlaylistEntry) -> String {
    entry.title.as_deref().map(client::sanitize_for_display).unwrap_or_else(|| "(untitled)".to_string())
}

// The id keeps two different videos with the same title from colliding,
// which yt-dlp would otherwise report as "already downloaded" and hand back
// the other video's file.
const SINGLE_FILENAME_TEMPLATE: &str = "%(title)s [%(id)s].%(ext)s";

// Zero-padded to the playlist's size so the files sort in playlist order.
pub fn playlist_filename_template(position: usize, total: usize) -> String {
    let width = total.max(1).to_string().len();
    format!("{position:0width$} - {SINGLE_FILENAME_TEMPLATE}")
}

fn add_playlist(
    socket: &Path,
    title: &str,
    entries: &[PlaylistEntry],
    truncated_from: Option<usize>,
    opts: &AddOptions,
) -> anyhow::Result<i32> {
    let config = load_config()?;
    let fallback_mood = opts.mood.as_deref();
    if let Some(m) = fallback_mood {
        if !config.moods.contains_key(m) {
            eprintln!("{}", unknown_mood_message(&config, m));
            return Ok(1);
        }
    }
    let total = entries.len();
    let title = client::sanitize_for_display(title);
    println!("playlist '{}': {total} entries", if title.is_empty() { "untitled" } else { &title });
    if let Some(full) = truncated_from {
        eprintln!("warning: the playlist has {full} entries; only the first {total} are considered");
    }

    let plans: Vec<EntryPlan> = entries.iter().map(|e| plan_playlist_entry(e, &config, fallback_mood)).collect();
    let downloadable = plans.iter().filter(|p| matches!(p, EntryPlan::Add { is_live: false, .. })).count();
    let wants_download = downloadable > 0
        && ask_download(opts, &format!("Download all {downloadable} playlist entries locally instead of streaming?"))?;
    let format = if wants_download { download_format(socket) } else { "" };

    let mut known: HashSet<String> =
        config.moods.values().flat_map(|m| m.sources.iter().cloned()).collect();
    let mut tally = Tally::default();
    for (entry, plan) in entries.iter().zip(plans) {
        let label = display_title(entry);
        let (url, mood, is_live) = match plan {
            EntryPlan::Add { url, mood, is_live } => (url, mood, is_live),
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
        let source = if wants_download && !is_live {
            match download_entry(entry, total, &url, &mood, format) {
                Ok(path) => path,
                Err(message) => {
                    eprintln!("warning: entry {} '{label}': {message}", entry.position);
                    tally.failed += 1;
                    continue;
                }
            }
        } else {
            if wants_download {
                eprintln!("note: entry {} '{label}' is a live stream; adding it as a streaming source", entry.position);
            }
            url
        };
        if !known.insert(source.clone()) {
            *tally.skipped.entry(SkipReason::AlreadyAdded).or_default() += 1;
            continue;
        }
        add_entry_source(socket, entry.position, total, &label, source, mood, &mut tally);
    }
    println!("{}", tally.summary(total));
    if tally.failed > 0 {
        eprintln!("warning: {} of {total} entries could not be added; the rest were added normally", tally.failed);
    }
    let had_work = tally.added_total() + tally.failed > 0;
    Ok(if had_work && tally.added_total() == 0 { 1 } else { 0 })
}

// One yt-dlp run per entry, so each file lands straight in its own mood's
// directory; a single playlist-wide run could only target one directory.
fn download_entry(entry: &PlaylistEntry, total: usize, url: &str, mood: &str, format: &str) -> Result<String, String> {
    let dir = prepare_mood_dir(mood).map_err(|e| e.to_string())?;
    let template = playlist_filename_template(entry.position, total);
    let outcome = client::download_audio(url, &dir, &template, format).map_err(|e| e.to_string())?;
    match outcome.saved.into_iter().next() {
        Some(path) => {
            if !outcome.exited_ok {
                eprintln!("warning: yt-dlp reported an error after saving {path}; adding the saved file anyway");
            }
            Ok(path)
        }
        None => Err("download failed, not added".to_string()),
    }
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

    fn opts(mood: Option<&str>) -> AddOptions {
        AddOptions { mood: mood.map(str::to_string), download: DownloadFlag::Ask, stdin_is_terminal: false }
    }

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
        let code = run(&env.socket, "https://www.youtube.com/playlist?list=PLx", &opts(None));
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
        let code = run(&env.socket, "https://www.youtube.com/playlist?list=PLx", &opts(Some("broken")));
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
        let code = run(&env.socket, "https://www.youtube.com/playlist?list=PLx", &opts(Some("nope")));
        teardown();
        assert_eq!(code.unwrap(), 1);
        assert!(adds(&env.received).is_empty());
    }

    #[test]
    fn a_single_video_url_is_added_as_the_url_itself() {
        let _guard = env_lock();
        let env = setup(r#"{"_type": "video", "title": "Heavy rain and thunder", "description": ""}"#);
        let code = run(&env.socket, "https://www.youtube.com/watch?v=aaaaaaaaaaa", &opts(None));
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
        let unclassified = run(&env.socket, "https://example.com/a", &opts(None));
        let explicit = run(&env.socket, "https://example.com/a", &opts(Some("ambient")));
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
        let with_mood = run(&env.socket, local.to_str().unwrap(), &opts(Some("ambient")));
        let without_mood = run(&env.socket, local.to_str().unwrap(), &opts(None));
        teardown();
        assert_eq!(with_mood.unwrap(), 0);
        assert_eq!(without_mood.unwrap(), 1, "a local path still needs --mood");
        assert_eq!(adds(&env.received), vec![(canonical, "ambient".to_string())]);
        assert!(!env.dir.path().join("yt-dlp-args").exists(), "yt-dlp was invoked for a local source");
    }

    #[test]
    fn download_decision_honors_flags_and_only_prompts_on_a_terminal() {
        use DownloadDecision as D;
        use DownloadFlag as F;
        for terminal in [true, false] {
            assert_eq!(download_decision(F::Download, terminal), D::Download);
            assert_eq!(download_decision(F::NoDownload, terminal), D::Stream);
        }
        assert_eq!(download_decision(F::Ask, true), D::Prompt);
        assert_eq!(download_decision(F::Ask, false), D::StreamNonInteractive);
    }

    #[test]
    fn only_an_explicit_yes_answer_downloads() {
        for yes in ["y", "Y", "yes", "YES", " yes\n", "y\n"] {
            assert!(is_yes(yes), "{yes:?}");
        }
        for no in ["", "\n", "n", "no", "yeah", "maybe", "y e s"] {
            assert!(!is_yes(no), "{no:?}");
        }
    }

    #[test]
    fn playlist_filenames_are_zero_padded_to_the_playlist_size() {
        assert_eq!(playlist_filename_template(3, 9), "3 - %(title)s [%(id)s].%(ext)s");
        assert_eq!(playlist_filename_template(3, 59), "03 - %(title)s [%(id)s].%(ext)s");
        assert_eq!(playlist_filename_template(42, 1000), "0042 - %(title)s [%(id)s].%(ext)s");
    }

    // Answers the playlist probe with `probe_json`, and otherwise simulates a
    // download into the -P directory, failing for URLs containing "fail".
    fn fake_yt_dlp_with_downloads(dir: &Path, probe_json: &str) -> std::path::PathBuf {
        use std::os::unix::fs::PermissionsExt;
        let probe_file = dir.join("probe.json");
        std::fs::write(&probe_file, probe_json).unwrap();
        let path = dir.join("fake-yt-dlp");
        let script = format!(
            r#"#!/bin/sh
case " $* " in *" --flat-playlist "*) cat '{probe}'; exit 0;; esac
printf '%s\n' "$*" >> '{log}'
prev=""; for a; do case "$prev" in -P) out="$a";; -o) tmpl="$a";; esac; prev="$a"; done
case "$prev" in *fail*) echo "ERROR: Private video" >&2; exit 1;; esac
id=$(printf '%s' "$prev" | sed 's/.*v=//')
name=$(printf '%s' "$tmpl" | sed "s/%(title)s/T/; s/%(id)s/$id/; s/%(ext)s/m4a/")
: > "$out/$name"
echo "LOFI-SAVED:$out/$name"
"#,
            probe = probe_file.display(),
            log = dir.join("downloads.log").display()
        );
        std::fs::write(&path, script).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        path
    }

    fn download_opts(mood: Option<&str>) -> AddOptions {
        AddOptions { mood: mood.map(str::to_string), download: DownloadFlag::Download, stdin_is_terminal: false }
    }

    fn data_path(env: &Env, rel: &str) -> String {
        std::fs::canonicalize(env.dir.path()).unwrap().join("data-home/lofi-launcher").join(rel).to_str().unwrap().to_string()
    }

    #[test]
    fn a_downloaded_single_video_is_added_as_its_local_file_in_its_mood_directory() {
        let _guard = env_lock();
        let env = setup("");
        let probe = r#"{"_type": "video", "title": "Heavy rain and thunder", "description": ""}"#;
        std::env::set_var("LOFI_YTDLP_BIN", fake_yt_dlp_with_downloads(env.dir.path(), probe));
        let code = run(&env.socket, "https://www.youtube.com/watch?v=aaaaaaaaaaa", &download_opts(None));
        teardown();
        assert_eq!(code.unwrap(), 0);
        assert_eq!(adds(&env.received), vec![(data_path(&env, "rainy-day/T [aaaaaaaaaaa].m4a"), "rainy-day".to_string())]);
        let log = std::fs::read_to_string(env.dir.path().join("downloads.log")).unwrap();
        assert!(log.contains("-f bestaudio/worst"), "the daemon's quality setting (max) was not used: {log}");
    }

    #[test]
    fn a_live_stream_is_streamed_even_when_download_is_requested() {
        let _guard = env_lock();
        let env = setup("");
        let probe = r#"{"_type": "video", "title": "lofi rain radio", "is_live": true}"#;
        std::env::set_var("LOFI_YTDLP_BIN", fake_yt_dlp_with_downloads(env.dir.path(), probe));
        let code = run(&env.socket, "https://www.youtube.com/watch?v=live", &download_opts(None));
        teardown();
        assert_eq!(code.unwrap(), 0);
        assert_eq!(adds(&env.received), vec![("https://www.youtube.com/watch?v=live".to_string(), "rainy-day".to_string())]);
        assert!(!env.dir.path().join("downloads.log").exists());
    }

    #[test]
    fn a_failed_single_download_adds_nothing() {
        let _guard = env_lock();
        let env = setup("");
        let probe = r#"{"_type": "video", "title": "Heavy rain", "description": ""}"#;
        std::env::set_var("LOFI_YTDLP_BIN", fake_yt_dlp_with_downloads(env.dir.path(), probe));
        let code = run(&env.socket, "https://www.youtube.com/watch?v=fail", &download_opts(None));
        teardown();
        assert_eq!(code.unwrap(), 1);
        assert!(adds(&env.received).is_empty());
    }

    #[test]
    fn a_downloaded_playlist_routes_each_entry_into_its_own_mood_directory_and_keeps_partial_results() {
        let _guard = env_lock();
        let env = setup("");
        let probe = r#"{"_type": "playlist", "title": "mixed", "entries": [
            {"ie_key": "Youtube", "id": "aaaaaaaaaaa", "url": "https://www.youtube.com/watch?v=aaaaaaaaaaa", "title": "Heavy rain"},
            {"ie_key": "Youtube", "id": "fffffffffff", "url": "https://www.youtube.com/watch?v=fail", "title": "Rain that was deleted"},
            {"ie_key": "Youtube", "id": "bbbbbbbbbbb", "url": "https://www.youtube.com/watch?v=bbbbbbbbbbb", "title": "Deep focus music"},
            {"ie_key": "Youtube", "id": "ccccccccccc", "url": "https://www.youtube.com/watch?v=ccccccccccc", "title": "chill beats radio", "live_status": "is_live"},
            {"ie_key": "Youtube", "id": "ddddddddddd", "url": "https://www.youtube.com/watch?v=ddddddddddd", "title": "Untitled 7"}
        ]}"#;
        std::env::set_var("LOFI_YTDLP_BIN", fake_yt_dlp_with_downloads(env.dir.path(), probe));
        let code = run(&env.socket, "https://www.youtube.com/playlist?list=PLx", &download_opts(Some("ambient")));
        teardown();
        assert_eq!(code.unwrap(), 0);
        assert_eq!(
            adds(&env.received),
            vec![
                (data_path(&env, "rainy-day/1 - T [aaaaaaaaaaa].m4a"), "rainy-day".to_string()),
                (data_path(&env, "deep-focus/3 - T [bbbbbbbbbbb].m4a"), "deep-focus".to_string()),
                ("https://www.youtube.com/watch?v=ccccccccccc".to_string(), "chill-beats".to_string()),
                (data_path(&env, "ambient/5 - T [ddddddddddd].m4a"), "ambient".to_string()),
            ]
        );
        let downloads = std::fs::read_to_string(env.dir.path().join("downloads.log")).unwrap();
        assert_eq!(downloads.lines().count(), 4, "one yt-dlp run per non-live entry: {downloads}");
    }

    #[test]
    fn download_flags_are_ignored_for_a_local_path() {
        let _guard = env_lock();
        let env = setup("");
        let local = env.dir.path().join("mix.flac");
        std::fs::write(&local, b"").unwrap();
        let code = run(&env.socket, local.to_str().unwrap(), &download_opts(Some("ambient")));
        teardown();
        assert_eq!(code.unwrap(), 0);
        assert_eq!(adds(&env.received).len(), 1);
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
