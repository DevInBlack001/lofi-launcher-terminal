use crate::mpv::MpvController;
use lofi_common::{Command, Config, Response};

pub struct DaemonState<M: MpvController> {
    config: Config,
    pub mpv: M,
    session_count: u32,
    current_mood: String,
    current_index: usize,
    playing: bool,
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
        }
    }

    pub fn session_count(&self) -> u32 {
        self.session_count
    }

    fn start_current_mood(&mut self) -> Response {
        let mood = match self.config.moods.get(&self.current_mood) {
            Some(m) => m,
            None => return Response::Error(format!("unknown mood: {}", self.current_mood)),
        };
        match mood.sources.get(self.current_index) {
            Some(source) => match self.mpv.start_source(source) {
                Ok(()) => {
                    self.playing = true;
                    Response::Ok
                }
                Err(e) => Response::Error(e.to_string()),
            },
            None => Response::Error(format!("mood '{}' has no sources configured", self.current_mood)),
        }
    }

    fn valid_mood_names(&self) -> String {
        self.config.moods.keys().cloned().collect::<Vec<_>>().join(", ")
    }

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
                self.current_index = (self.current_index + 1) % len;
                self.start_current_mood()
            }
            Command::Pause => match self.mpv.pause() {
                Ok(()) => Response::Ok,
                Err(e) => Response::Error(e.to_string()),
            },
            Command::Resume => match self.mpv.resume() {
                Ok(()) => Response::Ok,
                Err(e) => Response::Error(e.to_string()),
            },
            Command::Status => {
                let current_source = self
                    .config
                    .moods
                    .get(&self.current_mood)
                    .and_then(|m| m.sources.get(self.current_index))
                    .cloned();
                Response::Status {
                    mood: self.current_mood.clone(),
                    playing: self.playing,
                    current_source,
                }
            }
            Command::Moods => Response::Moods(self.config.moods.keys().cloned().collect()),
            Command::Reload => Response::Ok,
            Command::Add { source, mood } => {
                let target_mood = match mood {
                    Some(name) => {
                        if !self.config.moods.contains_key(&name) {
                            return Response::Error(format!(
                                "unknown mood '{name}', valid moods: {}",
                                self.valid_mood_names()
                            ));
                        }
                        name
                    }
                    None => {
                        return Response::Error(
                            "could not classify source without metadata; pass --mood explicitly \
                             (classification from a fetched title/description happens in the CLI \
                             before this command is sent)".to_string(),
                        );
                    }
                };
                self.config
                    .moods
                    .get_mut(&target_mood)
                    .expect("checked above")
                    .sources
                    .push(source);
                if let Err(e) = lofi_common::save_config(&lofi_common::config_path(), &self.config) {
                    return Response::Error(e.to_string());
                }
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
    }

    impl MpvController for FakeMpv {
        fn start_source(&mut self, source: &str) -> anyhow::Result<()> {
            self.started.push(source.to_string());
            self.stopped = false;
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
    fn add_with_unknown_explicit_mood_returns_error() {
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
