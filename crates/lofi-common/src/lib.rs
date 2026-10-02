pub mod config;
pub use config::{Config, Mood, BUILTIN_MOODS, config_path, load_config, load_config_or_default, save_config, default_config_toml};

pub mod protocol;
pub use protocol::{Command, Response, encode_command, decode_command, encode_response, decode_response};

pub mod classifier;
pub use classifier::classify;
