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
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", content = "data")]
pub enum Response {
    Ok,
    Error(String),
    Status {
        mood: String,
        playing: bool,
        current_source: Option<String>,
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
            playing: true,
            current_source: Some("https://example.com/playlist".to_string()),
        };
        let line = encode_response(&resp);
        let decoded = decode_response(line.trim_end()).unwrap();
        match decoded {
            Response::Status { mood, playing, current_source } => {
                assert_eq!(mood, "code-and-chill");
                assert!(playing);
                assert_eq!(current_source.as_deref(), Some("https://example.com/playlist"));
            }
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
