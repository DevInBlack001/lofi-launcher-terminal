use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::PathBuf;

pub const BUILTIN_MOODS: [&str; 5] = ["code-and-chill", "deep-focus", "chill-beats", "rainy-day", "ambient"];
pub const DEFAULT_LONG_SOURCE_MINUTES: u32 = 20;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Mood {
    #[serde(default)]
    pub sources: Vec<String>,
}

fn default_long_source_minutes() -> u32 {
    DEFAULT_LONG_SOURCE_MINUTES
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Config {
    pub default_mood: String,
    pub moods: BTreeMap<String, Mood>,
    #[serde(default)]
    pub classifier: BTreeMap<String, Vec<String>>,
    #[serde(default = "default_long_source_minutes")]
    pub long_source_minutes: u32,
}

pub fn default_config_toml() -> &'static str {
    include_str!("../../../config.default.toml")
}

pub fn config_path() -> PathBuf {
    let base = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            let home = std::env::var_os("HOME").expect("HOME must be set");
            PathBuf::from(home).join(".config")
        });
    base.join("lofi-launcher").join("config.toml")
}

pub fn load_config(path: &std::path::Path) -> anyhow::Result<Config> {
    let text = std::fs::read_to_string(path)?;
    let cfg: Config = toml::from_str(&text)?;
    Ok(cfg)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    #[test]
    fn parses_default_config_template() {
        let cfg: Config = toml::from_str(default_config_toml()).unwrap();
        assert_eq!(cfg.default_mood, "code-and-chill");
        for name in BUILTIN_MOODS {
            assert!(cfg.moods.contains_key(name), "missing mood {name}");
        }
    }

    #[test]
    fn load_config_reads_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        let mut f = std::fs::File::create(&path).unwrap();
        write!(f, "{}", default_config_toml()).unwrap();
        let cfg = load_config(&path).unwrap();
        assert_eq!(cfg.default_mood, "code-and-chill");
    }

    #[test]
    fn config_path_respects_xdg_config_home() {
        std::env::set_var("XDG_CONFIG_HOME", "/tmp/lofi-test-xdg");
        let path = config_path();
        assert_eq!(path, std::path::PathBuf::from("/tmp/lofi-test-xdg/lofi-launcher/config.toml"));
        std::env::remove_var("XDG_CONFIG_HOME");
    }
}
