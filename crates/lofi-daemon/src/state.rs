use crate::mpv::MpvController;
use lofi_common::{Command, Config, Response};

pub struct DaemonState<M: MpvController> {
    config: Config,
    pub mpv: M,
    session_count: u32,
    current_mood: String,
    current_index: usize,
    playing: bool,
    paused: bool,
    known_duration_seconds: Option<u64>,
}

impl<M: MpvController> DaemonState<M> {
    pub fn new(config: Config, mpv: M) -> Self {
        let current_mood = config.default_mood.clone();
        Self {
            config,
            mpv,
            session_count: 0,
            current_mood,
            current_index: 0,
            playing: false,
            paused: false,
            known_duration_seconds: None,
        }
    }

    // Accessor used by this project's own test suite, not by main.rs's runtime dispatch.
    #[allow(dead_code)]
    pub fn session_count(&self) -> u32 {
        self.session_count
    }

    fn start_current_mood(&mut self) -> Response {
        let mood = match self.config.moods.get(&self.current_mood) {
            Some(m) => m,
            None => return Response::Error(format!("unknown mood: {}", self.current_mood)),
        };
        match mood.sources.get(self.current_index) {
            Some(source) => {
                let threshold_seconds = (self.config.long_source_minutes as u64) * 60;
                let result = self.mpv.start_source_with_duration(
                    source,
                    self.known_duration_seconds,
                    threshold_seconds,
                );
                if let Err(e) = result {
                    return Response::Error(e.to_string());
                }
                self.playing = true;
                // mpv's pause property survives loadfile, so a source started after
                // an earlier pause would otherwise load silently paused.
                if let Err(e) = self.mpv.resume() {
                    return Response::Error(e.to_string());
                }
                self.paused = false;
                Response::Ok
            }
            None => Response::Error(format!("mood '{}' has no sources configured", self.current_mood)),
        }
    }

    #[cfg(test)]
    pub fn set_known_duration_seconds_for_test(&mut self, seconds: Option<u64>) {
        self.known_duration_seconds = seconds;
    }

    fn replace_config(&mut self, config: Config) {
        self.config = config;
        if !self.config.moods.contains_key(&self.current_mood) {
            self.current_mood = self.config.default_mood.clone();
            self.current_index = 0;
        }
    }

    fn valid_mood_names(&self) -> String {
        self.config.moods.keys().cloned().collect::<Vec<_>>().join(", ")
    }

    // Accessor used by this project's own test suite, not by main.rs's runtime dispatch.
    #[allow(dead_code)]
    pub fn mood_sources(&self, mood: &str) -> Vec<String> {
        self.config.moods.get(mood).map(|m| m.sources.clone()).unwrap_or_default()
    }

    pub fn handle(&mut self, cmd: Command) -> Response {
        match cmd {
            Command::Register => {
                self.session_count += 1;
                if self.session_count == 1 {
                    self.start_current_mood()
                } else {
                    Response::Ok
                }
            }
            Command::Unregister => {
                if self.session_count > 0 {
                    self.session_count -= 1;
                }
                if self.session_count == 0 && self.playing {
                    self.playing = false;
                    match self.mpv.stop() {
                        Ok(()) => Response::Ok,
                        Err(e) => Response::Error(e.to_string()),
                    }
                } else {
                    Response::Ok
                }
            }
            Command::Mood(name) => {
                if !self.config.moods.contains_key(&name) {
                    return Response::Error(format!(
                        "unknown mood '{name}', valid moods: {}",
                        self.valid_mood_names()
                    ));
                }
                self.current_mood = name;
                self.current_index = 0;
                if self.session_count > 0 {
                    self.start_current_mood()
                } else {
                    Response::Ok
                }
            }
            Command::Next => {
                let len = self.config.moods.get(&self.current_mood).map(|m| m.sources.len()).unwrap_or(0);
                if len == 0 {
                    return Response::Error(format!("mood '{}' has no sources configured", self.current_mood));
                }
                if self.session_count > 0 {
                    self.current_index = (self.current_index + 1) % len;
                    self.start_current_mood()
                } else {
                    Response::Ok
                }
            }
            Command::Pause => match self.mpv.pause() {
                Ok(()) => {
                    self.paused = true;
                    Response::Ok
                }
                Err(e) => Response::Error(e.to_string()),
            },
            Command::Resume => match self.mpv.resume() {
                Ok(()) => {
                    self.paused = false;
                    Response::Ok
                }
                Err(e) => Response::Error(e.to_string()),
            },
            Command::Status => {
                let current_source = self
                    .config
                    .moods
                    .get(&self.current_mood)
                    .and_then(|m| m.sources.get(self.current_index))
                    .cloned();
                let loaded = self.playing && self.session_count > 0;
                Response::Status {
                    mood: self.current_mood.clone(),
                    playing: loaded && !self.paused,
                    paused: loaded && self.paused,
                    current_source,
                }
            }
            Command::Moods => Response::Moods(self.config.moods.keys().cloned().collect()),
            Command::Reload => match lofi_common::load_config_or_default(&lofi_common::config_path()) {
                Ok(config) => {
                    self.replace_config(config);
                    Response::Ok
                }
                Err(e) => Response::Error(format!("could not reload config: {e}")),
            },
            Command::Add { source, mood } => {
                let target_mood = match mood {
                    Some(name) => name,
                    None => {
                        return Response::Error(
                            "could not classify source without metadata; pass --mood explicitly \
                             (classification from a fetched title/description happens in the CLI \
                             before this command is sent)".to_string(),
                        );
                    }
                };
                // The daemon is long-lived, so its in-memory config is likely stale
                // relative to hand edits; append to what is on disk right now.
                let path = lofi_common::config_path();
                let mut fresh = match lofi_common::load_config_or_default(&path) {
                    Ok(config) => config,
                    Err(e) => {
                        return Response::Error(format!(
                            "could not read {} (left untouched): {e}",
                            path.display()
                        ))
                    }
                };
                let Some(mood_entry) = fresh.moods.get_mut(&target_mood) else {
                    let valid = fresh.moods.keys().cloned().collect::<Vec<_>>().join(", ");
                    return Response::Error(format!("unknown mood '{target_mood}', valid moods: {valid}"));
                };
                mood_entry.sources.push(source);
                if let Err(e) = lofi_common::save_config(&path, &fresh) {
                    return Response::Error(e.to_string());
                }
                self.replace_config(fresh);
                Response::Classified(target_mood)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use lofi_common::Mood;
    use std::collections::BTreeMap;

    #[derive(Default)]
    struct FakeMpv {
        started: Vec<String>,
        stopped: bool,
        paused: bool,
        last_seek_requested: bool,
        loop_file_requested: bool,
    }

    impl MpvController for FakeMpv {
        fn start_source(&mut self, source: &str) -> anyhow::Result<()> {
            self.started.push(source.to_string());
            self.stopped = false;
            Ok(())
        }
        fn start_source_with_duration(
            &mut self,
            source: &str,
            duration_seconds: Option<u64>,
            long_source_threshold_seconds: u64,
        ) -> anyhow::Result<()> {
            self.started.push(source.to_string());
            self.stopped = false;
            self.last_seek_requested =
                matches!(duration_seconds, Some(d) if d > long_source_threshold_seconds);
            self.loop_file_requested = self.last_seek_requested;
            Ok(())
        }
        fn stop(&mut self) -> anyhow::Result<()> {
            self.stopped = true;
            Ok(())
        }
        fn pause(&mut self) -> anyhow::Result<()> {
            self.paused = true;
            Ok(())
        }
        fn resume(&mut self) -> anyhow::Result<()> {
            self.paused = false;
            Ok(())
        }
        fn last_error(&self) -> Option<String> {
            None
        }
        fn quit(&mut self) -> anyhow::Result<()> {
            Ok(())
        }
    }

    fn test_config() -> Config {
        let mut moods = BTreeMap::new();
        moods.insert("code-and-chill".to_string(), Mood { sources: vec!["a.mp3".to_string()] });
        moods.insert("deep-focus".to_string(), Mood { sources: vec![] });
        Config {
            default_mood: "code-and-chill".to_string(),
            moods,
            classifier: BTreeMap::new(),
            long_source_minutes: 20,
        }
    }

    #[test]
    fn register_starts_playback_only_on_first_session() {
        let mut state = DaemonState::new(test_config(), FakeMpv::default());
        assert!(matches!(state.handle(Command::Register), Response::Ok));
        assert_eq!(state.mpv.started, vec!["a.mp3".to_string()]);
        state.mpv.started.clear();
        assert!(matches!(state.handle(Command::Register), Response::Ok));
        assert!(state.mpv.started.is_empty(), "second register must not restart playback");
    }

    #[test]
    fn unregister_stops_playback_only_when_count_reaches_zero() {
        let mut state = DaemonState::new(test_config(), FakeMpv::default());
        state.handle(Command::Register);
        state.handle(Command::Register);
        state.handle(Command::Unregister);
        assert!(!state.mpv.stopped, "must not stop while one session remains");
        state.handle(Command::Unregister);
        assert!(state.mpv.stopped, "must stop when last session closes");
    }

    #[test]
    fn unregister_below_zero_is_a_no_op() {
        let mut state = DaemonState::new(test_config(), FakeMpv::default());
        assert!(matches!(state.handle(Command::Unregister), Response::Ok));
        assert_eq!(state.session_count(), 0);
    }

    #[test]
    fn mood_switch_to_unknown_mood_returns_error_listing_valid_names() {
        let mut state = DaemonState::new(test_config(), FakeMpv::default());
        let resp = state.handle(Command::Mood("not-a-mood".to_string()));
        match resp {
            Response::Error(msg) => {
                assert!(msg.contains("code-and-chill"));
                assert!(msg.contains("deep-focus"));
            }
            other => panic!("expected error, got {other:?}"),
        }
    }

    #[test]
    fn mood_with_empty_sources_reports_warning_instead_of_starting() {
        let mut state = DaemonState::new(test_config(), FakeMpv::default());
        state.handle(Command::Register);
        let resp = state.handle(Command::Mood("deep-focus".to_string()));
        match resp {
            Response::Error(msg) => assert!(msg.contains("no sources configured")),
            other => panic!("expected warning error, got {other:?}"),
        }
    }

    #[test]
    fn add_with_explicit_mood_appends_source_and_reports_classified_mood() {
        let _guard = crate::ENV_MUTEX.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        let scratch_dir = tempfile::tempdir().unwrap();
        std::env::set_var("XDG_CONFIG_HOME", scratch_dir.path());

        let mut state = DaemonState::new(test_config(), FakeMpv::default());
        let resp = state.handle(Command::Add {
            source: "https://example.com/mix.mp4".to_string(),
            mood: Some("deep-focus".to_string()),
        });
        match resp {
            Response::Classified(mood) => assert_eq!(mood, "deep-focus"),
            other => {
                std::env::remove_var("XDG_CONFIG_HOME");
                panic!("expected Classified, got {other:?}")
            }
        }
        assert!(state.mood_sources("deep-focus").contains(&"https://example.com/mix.mp4".to_string()));

        let written = std::fs::read_to_string(scratch_dir.path().join("lofi-launcher").join("config.toml")).unwrap();
        std::env::remove_var("XDG_CONFIG_HOME");
        assert!(written.contains("https://example.com/mix.mp4"), "config file did not contain the new source: {written}");
    }

    #[test]
    fn long_source_above_threshold_requests_a_seek() {
        let mut moods = BTreeMap::new();
        moods.insert(
            "ambient".to_string(),
            Mood { sources: vec!["https://example.com/3hour-mix.mp4".to_string()] },
        );
        let config = Config {
            default_mood: "ambient".to_string(),
            moods,
            classifier: BTreeMap::new(),
            long_source_minutes: 20,
        };
        let mut state = DaemonState::new(config, FakeMpv::default());
        state.set_known_duration_seconds_for_test(Some(3 * 3600));
        state.handle(Command::Register);
        assert!(state.mpv.last_seek_requested, "a 3 hour source must request a random seek");
    }

    #[test]
    fn short_source_does_not_request_a_seek() {
        let mut moods = BTreeMap::new();
        moods.insert(
            "ambient".to_string(),
            Mood { sources: vec!["short-track.mp3".to_string()] },
        );
        let config = Config {
            default_mood: "ambient".to_string(),
            moods,
            classifier: BTreeMap::new(),
            long_source_minutes: 20,
        };
        let mut state = DaemonState::new(config, FakeMpv::default());
        state.set_known_duration_seconds_for_test(Some(180));
        state.handle(Command::Register);
        assert!(!state.mpv.last_seek_requested, "a 3 minute source must not request a seek");
        assert!(!state.mpv.loop_file_requested, "a 3 minute source must not request loop-file");
    }

    fn status_flags(state: &mut DaemonState<FakeMpv>) -> (bool, bool) {
        match state.handle(Command::Status) {
            Response::Status { playing, paused, .. } => (playing, paused),
            other => panic!("unexpected {other:?}"),
        }
    }

    #[test]
    fn status_reports_stopped_before_any_session_registers() {
        let mut state = DaemonState::new(test_config(), FakeMpv::default());
        assert_eq!(status_flags(&mut state), (false, false));
    }

    #[test]
    fn pause_and_resume_are_reflected_in_status() {
        let mut state = DaemonState::new(test_config(), FakeMpv::default());
        state.handle(Command::Register);
        assert_eq!(status_flags(&mut state), (true, false));
        assert!(matches!(state.handle(Command::Pause), Response::Ok));
        assert_eq!(status_flags(&mut state), (false, true));
        assert!(matches!(state.handle(Command::Resume), Response::Ok));
        assert_eq!(status_flags(&mut state), (true, false));
    }

    #[test]
    fn status_reports_stopped_after_last_session_unregisters_while_paused() {
        let mut state = DaemonState::new(test_config(), FakeMpv::default());
        state.handle(Command::Register);
        state.handle(Command::Pause);
        state.handle(Command::Unregister);
        assert_eq!(status_flags(&mut state), (false, false));
    }

    #[test]
    fn fresh_source_start_always_unpauses_mpv() {
        let mut state = DaemonState::new(test_config(), FakeMpv::default());
        state.handle(Command::Register);
        state.handle(Command::Pause);
        state.handle(Command::Unregister);
        assert!(state.mpv.paused, "fake mpv keeps its pause flag across stop, like real mpv");
        state.handle(Command::Register);
        assert!(!state.mpv.paused, "new source must not inherit the old pause flag");
        assert_eq!(status_flags(&mut state), (true, false));
    }

    #[test]
    fn next_with_no_registered_session_does_not_start_playback_or_advance_index() {
        let mut state = DaemonState::new(test_config(), FakeMpv::default());
        let resp = state.handle(Command::Next);
        assert!(matches!(resp, Response::Ok), "unexpected {resp:?}");
        assert!(state.mpv.started.is_empty(), "must not talk to mpv with no registered session");
        assert_eq!(state.current_index, 0, "must not advance the index with no registered session");
    }

    #[test]
    fn next_while_paused_starts_unpaused() {
        let mut state = DaemonState::new(test_config(), FakeMpv::default());
        state.handle(Command::Register);
        state.handle(Command::Pause);
        state.handle(Command::Next);
        assert!(!state.mpv.paused);
        assert_eq!(status_flags(&mut state), (true, false));
    }

    fn write_config_to(dir: &std::path::Path, config: &Config) -> std::path::PathBuf {
        let path = dir.join("lofi-launcher").join("config.toml");
        lofi_common::save_config(&path, config).unwrap();
        path
    }

    #[test]
    fn reload_picks_up_sources_edited_on_disk() {
        let _guard = crate::ENV_MUTEX.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        let scratch_dir = tempfile::tempdir().unwrap();
        std::env::set_var("XDG_CONFIG_HOME", scratch_dir.path());

        let mut state = DaemonState::new(test_config(), FakeMpv::default());
        let mut edited = test_config();
        edited.moods.get_mut("deep-focus").unwrap().sources.push("hand-edited.mp3".to_string());
        write_config_to(scratch_dir.path(), &edited);

        let resp = state.handle(Command::Reload);
        std::env::remove_var("XDG_CONFIG_HOME");
        assert!(matches!(resp, Response::Ok), "unexpected {resp:?}");
        assert_eq!(state.mood_sources("deep-focus"), vec!["hand-edited.mp3".to_string()]);
    }

    #[test]
    fn reload_falls_back_to_default_config_when_file_is_missing() {
        let _guard = crate::ENV_MUTEX.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        let scratch_dir = tempfile::tempdir().unwrap();
        std::env::set_var("XDG_CONFIG_HOME", scratch_dir.path());

        let mut state = DaemonState::new(test_config(), FakeMpv::default());
        let resp = state.handle(Command::Reload);
        std::env::remove_var("XDG_CONFIG_HOME");
        assert!(matches!(resp, Response::Ok), "unexpected {resp:?}");
        assert!(state.mood_sources("code-and-chill").is_empty(), "default config ships empty moods");
    }

    #[test]
    fn reload_resets_to_default_mood_when_current_mood_was_removed() {
        let _guard = crate::ENV_MUTEX.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        let scratch_dir = tempfile::tempdir().unwrap();
        std::env::set_var("XDG_CONFIG_HOME", scratch_dir.path());

        let mut state = DaemonState::new(test_config(), FakeMpv::default());
        state.handle(Command::Mood("deep-focus".to_string()));
        let mut edited = test_config();
        edited.moods.remove("deep-focus");
        write_config_to(scratch_dir.path(), &edited);

        state.handle(Command::Reload);
        let status = state.handle(Command::Status);
        std::env::remove_var("XDG_CONFIG_HOME");
        match status {
            Response::Status { mood, .. } => assert_eq!(mood, "code-and-chill"),
            other => panic!("unexpected {other:?}"),
        }
        assert_eq!(state.current_index, 0);
    }

    #[test]
    fn reload_keeps_sessions_and_playback_running() {
        let _guard = crate::ENV_MUTEX.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        let scratch_dir = tempfile::tempdir().unwrap();
        std::env::set_var("XDG_CONFIG_HOME", scratch_dir.path());

        let mut state = DaemonState::new(test_config(), FakeMpv::default());
        state.handle(Command::Register);
        write_config_to(scratch_dir.path(), &test_config());

        state.handle(Command::Reload);
        std::env::remove_var("XDG_CONFIG_HOME");
        assert_eq!(state.session_count(), 1);
        assert!(state.playing);
        assert!(!state.mpv.stopped, "reload must not stop playback");
    }

    #[test]
    fn reload_with_malformed_config_returns_error_and_keeps_old_config() {
        let _guard = crate::ENV_MUTEX.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        let scratch_dir = tempfile::tempdir().unwrap();
        std::env::set_var("XDG_CONFIG_HOME", scratch_dir.path());
        let path = scratch_dir.path().join("lofi-launcher").join("config.toml");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, "this is = = not toml").unwrap();

        let mut state = DaemonState::new(test_config(), FakeMpv::default());
        let resp = state.handle(Command::Reload);
        std::env::remove_var("XDG_CONFIG_HOME");
        assert!(matches!(resp, Response::Error(_)), "unexpected {resp:?}");
        assert_eq!(state.mood_sources("code-and-chill"), vec!["a.mp3".to_string()]);
    }

    #[test]
    fn add_rereads_config_from_disk_and_preserves_hand_edits() {
        let _guard = crate::ENV_MUTEX.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        let scratch_dir = tempfile::tempdir().unwrap();
        std::env::set_var("XDG_CONFIG_HOME", scratch_dir.path());

        // Daemon starts with the stale in-memory snapshot...
        let mut state = DaemonState::new(test_config(), FakeMpv::default());
        // ...then the user hand-edits the file while the daemon keeps running.
        let mut edited = test_config();
        edited.moods.get_mut("code-and-chill").unwrap().sources.push("hand-edited.mp3".to_string());
        edited.long_source_minutes = 45;
        let path = write_config_to(scratch_dir.path(), &edited);

        let resp = state.handle(Command::Add {
            source: "https://example.com/new.mp4".to_string(),
            mood: Some("deep-focus".to_string()),
        });
        let written = lofi_common::load_config(&path);
        std::env::remove_var("XDG_CONFIG_HOME");
        assert!(matches!(resp, Response::Classified(ref m) if m == "deep-focus"), "unexpected {resp:?}");

        let written = written.unwrap();
        assert_eq!(
            written.moods["code-and-chill"].sources,
            vec!["a.mp3".to_string(), "hand-edited.mp3".to_string()],
            "hand edit was clobbered"
        );
        assert_eq!(written.long_source_minutes, 45);
        assert_eq!(written.moods["deep-focus"].sources, vec!["https://example.com/new.mp4".to_string()]);
        // In-memory view must match disk without needing a separate reload.
        assert_eq!(state.mood_sources("code-and-chill"), written.moods["code-and-chill"].sources);
        assert_eq!(state.mood_sources("deep-focus"), written.moods["deep-focus"].sources);
    }

    #[test]
    fn add_accepts_a_mood_that_only_exists_in_the_on_disk_config() {
        let _guard = crate::ENV_MUTEX.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        let scratch_dir = tempfile::tempdir().unwrap();
        std::env::set_var("XDG_CONFIG_HOME", scratch_dir.path());

        let mut state = DaemonState::new(test_config(), FakeMpv::default());
        let mut edited = test_config();
        edited.moods.insert("night-drive".to_string(), Mood { sources: vec![] });
        write_config_to(scratch_dir.path(), &edited);

        let resp = state.handle(Command::Add {
            source: "drive.mp3".to_string(),
            mood: Some("night-drive".to_string()),
        });
        std::env::remove_var("XDG_CONFIG_HOME");
        assert!(matches!(resp, Response::Classified(ref m) if m == "night-drive"), "unexpected {resp:?}");
        assert_eq!(state.mood_sources("night-drive"), vec!["drive.mp3".to_string()]);
    }

    #[test]
    fn add_with_malformed_on_disk_config_refuses_to_overwrite_it() {
        let _guard = crate::ENV_MUTEX.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        let scratch_dir = tempfile::tempdir().unwrap();
        std::env::set_var("XDG_CONFIG_HOME", scratch_dir.path());
        let path = scratch_dir.path().join("lofi-launcher").join("config.toml");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, "this is = = not toml").unwrap();

        let mut state = DaemonState::new(test_config(), FakeMpv::default());
        let resp = state.handle(Command::Add {
            source: "x.mp3".to_string(),
            mood: Some("deep-focus".to_string()),
        });
        let on_disk = std::fs::read_to_string(&path).unwrap();
        std::env::remove_var("XDG_CONFIG_HOME");
        assert!(matches!(resp, Response::Error(_)), "unexpected {resp:?}");
        assert_eq!(on_disk, "this is = = not toml");
    }

    #[test]
    fn add_with_unknown_explicit_mood_returns_error() {
        let _guard = crate::ENV_MUTEX.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        let scratch_dir = tempfile::tempdir().unwrap();
        std::env::set_var("XDG_CONFIG_HOME", scratch_dir.path());

        let mut state = DaemonState::new(test_config(), FakeMpv::default());
        let resp = state.handle(Command::Add {
            source: "a.mp3".to_string(),
            mood: Some("not-a-mood".to_string()),
        });

        std::env::remove_var("XDG_CONFIG_HOME");
        assert!(matches!(resp, Response::Error(_)));
    }
}
