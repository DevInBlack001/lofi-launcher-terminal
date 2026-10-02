pub mod config;
pub use config::{Config, Mood, BUILTIN_MOODS, AUDIO_QUALITIES, DEFAULT_AUDIO_QUALITY, ytdl_format_for, is_valid_audio_quality, config_path, xdg_absolute_dir, load_config, load_config_or_default, save_config, default_config_toml};

pub mod protocol;
pub use protocol::{Command, Response, encode_command, decode_command, encode_response, decode_response};

pub mod classifier;
pub use classifier::classify;

// One lock for every test in this crate that mutates process-wide env vars.
#[cfg(test)]
pub(crate) static ENV_MUTEX: std::sync::Mutex<()> = std::sync::Mutex::new(());
