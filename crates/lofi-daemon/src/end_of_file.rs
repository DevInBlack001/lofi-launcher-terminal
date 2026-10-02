use crate::mpv::RealMpv;
use crate::state::DaemonState;
use std::io::{BufRead, BufReader, Read, Write};
use std::os::unix::net::UnixStream;
use std::sync::atomic::Ordering;
use std::sync::{Arc, Mutex};

// One event is one short JSON line; anything far larger is not mpv.
const MAX_EVENT_LINE_BYTES: u64 = 64 * 1024;
const IDLE_ACTIVE_OBSERVER_ID: u64 = 1;

#[derive(Debug, PartialEq, Eq)]
pub(crate) enum PlaybackEnd {
    Natural,
    Failed,
}

// mpv reports why a file ended in its end-file event, but only going idle
// afterwards means nothing else is queued (a playlist URL expands into many
// entries that each end with "eof" while mpv moves straight on to the next).
// So the reason is remembered here and only acted on once mpv goes idle.
#[derive(Default)]
pub(crate) struct EndTracker {
    pending: Option<(String, u64)>,
}

impl EndTracker {
    // Returns what ended and the source generation at the moment its end-file
    // event arrived, once mpv has gone idle after it.
    pub(crate) fn on_event(&mut self, event: &serde_json::Value, generation: u64) -> Option<(PlaybackEnd, u64)> {
        match event.get("event").and_then(|e| e.as_str()) {
            Some("start-file") => {
                self.pending = None;
                None
            }
            Some("end-file") => {
                let reason = event.get("reason").and_then(|r| r.as_str()).unwrap_or("").to_string();
                self.pending = Some((reason, generation));
                None
            }
            Some("property-change")
                if event.get("id").and_then(|id| id.as_u64()) == Some(IDLE_ACTIVE_OBSERVER_ID)
                    && event.get("data").and_then(|d| d.as_bool()) == Some(true) =>
            {
                let (reason, generation_at_end) = self.pending.take()?;
                match reason.as_str() {
                    "eof" => Some((PlaybackEnd::Natural, generation_at_end)),
                    "error" => Some((PlaybackEnd::Failed, generation_at_end)),
                    // "stop", "quit", "redirect": the daemon or mpv itself moved
                    // on deliberately, so there is nothing to react to.
                    _ => None,
                }
            }
            _ => None,
        }
    }
}

// Started once, after the daemon state exists, on its own long-lived
// connection: the request/response connections RealMpv uses for commands
// close after a single reply and would miss events.
pub fn spawn_end_of_file_listener(daemon_state: Arc<Mutex<DaemonState<RealMpv>>>) -> anyhow::Result<()> {
    let (socket_path, generation) = {
        let guard = daemon_state.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        (guard.mpv.socket_path().to_path_buf(), guard.mpv.generation_counter())
    };
    let mut stream = UnixStream::connect(&socket_path)?;
    let observe = serde_json::json!({ "command": ["observe_property", IDLE_ACTIVE_OBSERVER_ID, "idle-active"] });
    stream.write_all(format!("{observe}\n").as_bytes())?;

    std::thread::spawn(move || {
        let mut reader = BufReader::new(stream);
        let mut tracker = EndTracker::default();
        loop {
            let mut line = String::new();
            match (&mut reader).take(MAX_EVENT_LINE_BYTES).read_line(&mut line) {
                // mpv exited (daemon shutdown); nothing left to listen to.
                Ok(0) | Err(_) => return,
                Ok(_) => {}
            }
            let Ok(event) = serde_json::from_str::<serde_json::Value>(&line) else { continue };
            let Some((end, generation_at_end)) = tracker.on_event(&event, generation.load(Ordering::SeqCst)) else {
                continue;
            };
            let mut guard = daemon_state.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
            // A user command started a different source after this event
            // arrived; the end belongs to a source that is already gone.
            if generation.load(Ordering::SeqCst) != generation_at_end {
                continue;
            }
            // Catches a user command that loaded a new source before this
            // listener got around to reading the event. Holding the state lock
            // keeps any further command from slipping in before we act.
            if !matches!(guard.mpv.is_idle(), Ok(true)) {
                continue;
            }
            match end {
                PlaybackEnd::Natural => {
                    let _ = guard.handle_natural_end_of_file();
                }
                PlaybackEnd::Failed => guard.handle_source_failed(),
            }
        }
    });
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn feed(tracker: &mut EndTracker, lines: &[&str]) -> Vec<(PlaybackEnd, u64)> {
        lines
            .iter()
            .filter_map(|line| tracker.on_event(&serde_json::from_str(line).unwrap(), 7))
            .collect()
    }

    // Event sequences below are copied from a real mpv 0.41 IPC session.
    #[test]
    fn natural_end_is_reported_once_mpv_goes_idle() {
        let mut tracker = EndTracker::default();
        let ends = feed(
            &mut tracker,
            &[
                r#"{"event":"start-file","playlist_entry_id":2}"#,
                r#"{"event":"file-loaded"}"#,
                r#"{"event":"end-file","reason":"eof","playlist_entry_id":2}"#,
                r#"{"event":"idle"}"#,
                r#"{"event":"property-change","id":1,"name":"idle-active","data":true}"#,
            ],
        );
        assert_eq!(ends, vec![(PlaybackEnd::Natural, 7)]);
    }

    #[test]
    fn a_source_replaced_by_a_user_command_is_not_a_natural_end() {
        let mut tracker = EndTracker::default();
        let ends = feed(
            &mut tracker,
            &[
                r#"{"event":"end-file","reason":"stop","playlist_entry_id":1}"#,
                r#"{"event":"start-file","playlist_entry_id":2}"#,
                r#"{"event":"property-change","id":1,"name":"idle-active","data":false}"#,
            ],
        );
        assert!(ends.is_empty());
    }

    #[test]
    fn an_explicit_stop_going_idle_is_not_a_natural_end() {
        let mut tracker = EndTracker::default();
        let ends = feed(
            &mut tracker,
            &[
                r#"{"event":"end-file","reason":"stop","playlist_entry_id":3}"#,
                r#"{"event":"property-change","id":1,"name":"idle-active","data":true}"#,
            ],
        );
        assert!(ends.is_empty());
    }

    #[test]
    fn a_failed_load_is_reported_as_a_failure_not_a_natural_end() {
        let mut tracker = EndTracker::default();
        let ends = feed(
            &mut tracker,
            &[
                r#"{"event":"start-file","playlist_entry_id":4}"#,
                r#"{"event":"end-file","reason":"error","playlist_entry_id":4,"file_error":"loading failed"}"#,
                r#"{"event":"idle"}"#,
                r#"{"event":"property-change","id":1,"name":"idle-active","data":true}"#,
            ],
        );
        assert_eq!(ends, vec![(PlaybackEnd::Failed, 7)]);
    }

    #[test]
    fn startup_idle_and_unrelated_or_malformed_events_are_ignored() {
        let mut tracker = EndTracker::default();
        let ends = feed(
            &mut tracker,
            &[
                r#"{"request_id":0,"error":"success"}"#,
                r#"{"event":"property-change","id":1,"name":"idle-active","data":true}"#,
                r#"{"event":"end-file"}"#,
                r#"{"event":"property-change","id":2,"name":"idle-active","data":true}"#,
                r#"{"event":"property-change","id":1,"name":"idle-active","data":null}"#,
            ],
        );
        assert!(ends.is_empty());
    }

    #[test]
    fn a_playlist_entry_ending_while_mpv_moves_to_the_next_entry_is_not_reported() {
        let mut tracker = EndTracker::default();
        let ends = feed(
            &mut tracker,
            &[
                r#"{"event":"end-file","reason":"eof","playlist_entry_id":5}"#,
                r#"{"event":"start-file","playlist_entry_id":6}"#,
                r#"{"event":"end-file","reason":"eof","playlist_entry_id":6}"#,
                r#"{"event":"property-change","id":1,"name":"idle-active","data":true}"#,
            ],
        );
        assert_eq!(ends, vec![(PlaybackEnd::Natural, 7)], "only the final entry going idle counts");
    }

    #[test]
    fn records_the_generation_from_when_the_end_arrived() {
        let mut tracker = EndTracker::default();
        let end: serde_json::Value = serde_json::from_str(r#"{"event":"end-file","reason":"eof"}"#).unwrap();
        let idle: serde_json::Value =
            serde_json::from_str(r#"{"event":"property-change","id":1,"name":"idle-active","data":true}"#).unwrap();
        assert_eq!(tracker.on_event(&end, 3), None);
        assert_eq!(tracker.on_event(&idle, 4), Some((PlaybackEnd::Natural, 3)));
    }
}
