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

pub fn load_config_or_default(path: &std::path::Path) -> anyhow::Result<Config> {
    if path.exists() {
        load_config(path)
    } else {
        Ok(toml::from_str(default_config_toml())?)
    }
}

pub fn save_config(path: &std::path::Path, config: &Config) -> anyhow::Result<()> {
    use std::io::Write;
    let parent = path
        .parent()
        .ok_or_else(|| anyhow::anyhow!("config path {} has no parent directory", path.display()))?;
    std::fs::create_dir_all(parent)?;
    let text = toml::to_string_pretty(config)?;
    let file_name = path
        .file_name()
        .ok_or_else(|| anyhow::anyhow!("config path {} has no file name", path.display()))?;
    // Same directory as the target so the rename stays on one filesystem and is
    // atomic: a crash mid-write leaves the old config intact, never a truncated one.
    let tmp_path = parent.join(format!(".{}.tmp-{}", file_name.to_string_lossy(), std::process::id()));
    let write_result = (|| -> anyhow::Result<()> {
        let mut file = std::fs::File::create(&tmp_path)?;
        file.write_all(text.as_bytes())?;
        file.sync_all()?;
        std::fs::rename(&tmp_path, path)?;
        Ok(())
    })();
    if write_result.is_err() {
        let _ = std::fs::remove_file(&tmp_path);
    }
    write_result
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
    fn default_classifier_sends_deep_focus_titles_to_deep_focus() {
        let cfg: Config = toml::from_str(default_config_toml()).unwrap();
        assert_eq!(
            crate::classify(&cfg.classifier, "Deep Focus Music - 3 hours", ""),
            Some("deep-focus".to_string())
        );
        assert_eq!(
            crate::classify(&cfg.classifier, "Synthwave mix to code to", ""),
            Some("code-and-chill".to_string())
        );
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
    fn save_config_writes_atomically_without_leaving_temp_files() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("nested").join("config.toml");
        let mut cfg: Config = toml::from_str(default_config_toml()).unwrap();
        cfg.moods.get_mut("ambient").unwrap().sources.push("/music/a.flac".to_string());
        save_config(&path, &cfg).unwrap();
        save_config(&path, &cfg).unwrap();

        let reloaded = load_config(&path).unwrap();
        assert_eq!(reloaded.moods["ambient"].sources, vec!["/music/a.flac".to_string()]);
        let entries: Vec<_> = std::fs::read_dir(path.parent().unwrap())
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().to_string())
            .collect();
        assert_eq!(entries, vec!["config.toml".to_string()]);
    }

    #[test]
    fn load_config_or_default_falls_back_when_file_is_missing() {
        let dir = tempfile::tempdir().unwrap();
        let cfg = load_config_or_default(&dir.path().join("missing.toml")).unwrap();
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
