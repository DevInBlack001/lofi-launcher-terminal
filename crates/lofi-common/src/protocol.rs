use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "cmd", content = "arg")]
pub enum Command {
    Register,
    Unregister,
    Mood(String),
    Next,
    Pause,
    Resume,
    Status,
    Moods,
    Reload,
    Add { source: String, mood: Option<String> },
    SetLoop(bool),
    SetAudioQuality(String),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", content = "data")]
pub enum Response {
    Ok,
    Error(String),
    Status {
        mood: String,
        // True only while audibly playing: a session is registered, a source
        // is loaded, and it is not paused.
        playing: bool,
        // Defaulted so a CLI talking to a daemon built before this field
        // existed still decodes its status.
        #[serde(default)]
        paused: bool,
        current_source: Option<String>,
        // A daemon predating this field never auto-advanced, so false is accurate.
        #[serde(default)]
        loop_playback: bool,
        // Empty when talking to a daemon predating audio quality selection.
        #[serde(default)]
        audio_quality: String,
    },
    Moods(Vec<String>),
    Classified(String),
}

pub fn encode_command(cmd: &Command) -> String {
    format!("{}\n", serde_json::to_string(cmd).expect("Command always serializes"))
}

pub fn decode_command(line: &str) -> anyhow::Result<Command> {
    Ok(serde_json::from_str(line)?)
}

pub fn encode_response(resp: &Response) -> String {
    format!("{}\n", serde_json::to_string(resp).expect("Response always serializes"))
}

pub fn decode_response(line: &str) -> anyhow::Result<Response> {
    Ok(serde_json::from_str(line)?)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips_mood_command() {
        let cmd = Command::Mood("deep-focus".to_string());
        let line = encode_command(&cmd);
        assert!(line.ends_with('\n'));
        let decoded = decode_command(line.trim_end()).unwrap();
        match decoded {
            Command::Mood(name) => assert_eq!(name, "deep-focus"),
            other => panic!("unexpected {other:?}"),
        }
    }

    #[test]
    fn round_trips_status_response() {
        let resp = Response::Status {
            mood: "code-and-chill".to_string(),
            playing: false,
            paused: true,
            current_source: Some("https://example.com/playlist".to_string()),
            loop_playback: true,
            audio_quality: "max".to_string(),
        };
        let line = encode_response(&resp);
        let decoded = decode_response(line.trim_end()).unwrap();
        match decoded {
            Response::Status { mood, playing, paused, current_source, loop_playback, audio_quality } => {
                assert_eq!(mood, "code-and-chill");
                assert!(!playing);
                assert!(paused);
                assert_eq!(current_source.as_deref(), Some("https://example.com/playlist"));
                assert!(loop_playback);
                assert_eq!(audio_quality, "max");
            }
            other => panic!("unexpected {other:?}"),
        }
    }

    #[test]
    fn decodes_status_from_a_daemon_that_predates_the_paused_field() {
        let line = r#"{"kind":"Status","data":{"mood":"ambient","playing":true,"current_source":null}}"#;
        match decode_response(line).unwrap() {
            Response::Status { playing, paused, loop_playback, audio_quality, .. } => {
                assert!(playing);
                assert!(!paused);
                assert!(!loop_playback);
                assert_eq!(audio_quality, "");
            }
            other => panic!("unexpected {other:?}"),
        }
    }

    #[test]
    fn round_trips_set_loop_command() {
        for enabled in [true, false] {
            let line = encode_command(&Command::SetLoop(enabled));
            match decode_command(line.trim_end()).unwrap() {
                Command::SetLoop(decoded) => assert_eq!(decoded, enabled),
                other => panic!("unexpected {other:?}"),
            }
        }
    }

    #[test]
    fn round_trips_set_audio_quality_command() {
        let line = encode_command(&Command::SetAudioQuality("max".to_string()));
        match decode_command(line.trim_end()).unwrap() {
            Command::SetAudioQuality(quality) => assert_eq!(quality, "max"),
            other => panic!("unexpected {other:?}"),
        }
    }

    #[test]
    fn rejects_malformed_line() {
        assert!(decode_command("not json").is_err());
    }

    #[test]
    fn round_trips_error_response() {
        let resp = Response::Error("something went wrong".to_string());
        let line = encode_response(&resp);
        let decoded = decode_response(line.trim_end()).unwrap();
        match decoded {
            Response::Error(msg) => assert_eq!(msg, "something went wrong"),
            other => panic!("unexpected {other:?}"),
        }
    }

    #[test]
    fn round_trips_moods_response() {
        let resp = Response::Moods(vec!["code-and-chill".to_string(), "ambient".to_string()]);
        let line = encode_response(&resp);
        let decoded = decode_response(line.trim_end()).unwrap();
        match decoded {
            Response::Moods(names) => assert_eq!(names, vec!["code-and-chill".to_string(), "ambient".to_string()]),
            other => panic!("unexpected {other:?}"),
        }
    }

    #[test]
    fn round_trips_classified_response() {
        let resp = Response::Classified("rainy-day".to_string());
        let line = encode_response(&resp);
        let decoded = decode_response(line.trim_end()).unwrap();
        match decoded {
            Response::Classified(mood) => assert_eq!(mood, "rainy-day"),
            other => panic!("unexpected {other:?}"),
        }
    }
}
