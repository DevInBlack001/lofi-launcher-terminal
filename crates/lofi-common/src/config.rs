use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::PathBuf;

pub const BUILTIN_MOODS: [&str; 5] = ["code-and-chill", "deep-focus", "chill-beats", "rainy-day", "ambient"];
pub const DEFAULT_LONG_SOURCE_MINUTES: u32 = 20;
pub const AUDIO_QUALITIES: [&str; 2] = ["min", "max"];
pub const DEFAULT_AUDIO_QUALITY: &str = "min";

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Mood {
    #[serde(default)]
    pub sources: Vec<String>,
}

fn default_long_source_minutes() -> u32 {
    DEFAULT_LONG_SOURCE_MINUTES
}

fn default_loop_playback() -> bool {
    true
}

fn default_audio_quality() -> String {
    DEFAULT_AUDIO_QUALITY.to_string()
}

pub fn is_valid_audio_quality(quality: &str) -> bool {
    AUDIO_QUALITIES.contains(&quality)
}

// Both tiers pick an audio-only stream whenever the source offers one, so no
// video bytes are fetched. The "/worst" fallback only engages for sources with
// no audio-only format at all (some live streams only publish muxed renditions):
// they still play, and "max" deliberately falls back to the smallest muxed
// stream rather than the largest, since the extra video bytes buy nothing.
pub fn ytdl_format_for(quality: &str) -> &'static str {
    match quality {
        "max" => "bestaudio/worst",
        // Covers "min" and any unrecognized value from a hand-edited config.
        _ => "worstaudio/worst",
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Config {
    pub default_mood: String,
    pub moods: BTreeMap<String, Mood>,
    #[serde(default)]
    pub classifier: BTreeMap<String, Vec<String>>,
    #[serde(default = "default_long_source_minutes")]
    pub long_source_minutes: u32,
    #[serde(default = "default_loop_playback")]
    pub loop_playback: bool,
    #[serde(default = "default_audio_quality")]
    pub audio_quality: String,
}

pub fn default_config_toml() -> &'static str {
    include_str!("../../../config.default.toml")
}

// The XDG base directory spec requires these variables to hold absolute paths
// and says anything else must be ignored. An empty XDG_RUNTIME_DIR in
// particular would put the sockets (mpv's accepts a "run" command) in the
// current directory instead of the user-only runtime dir.
pub fn xdg_absolute_dir(var: &str) -> Option<PathBuf> {
    let value = std::env::var_os(var)?;
    let path = PathBuf::from(value);
    if path.is_absolute() {
        Some(path)
    } else {
        None
    }
}

pub fn config_path() -> PathBuf {
    let base = xdg_absolute_dir("XDG_CONFIG_HOME")
        .unwrap_or_else(|| {
            let home = std::env::var_os("HOME").expect("HOME must be set");
            PathBuf::from(home).join(".config")
        });
    base.join("lofi-launcher").join("config.toml")
}

pub fn data_dir() -> PathBuf {
    let base = xdg_absolute_dir("XDG_DATA_HOME")
        .unwrap_or_else(|| {
            let home = std::env::var_os("HOME").expect("HOME must be set");
            PathBuf::from(home).join(".local").join("share")
        });
    base.join("lofi-launcher")
}

// Mood names come from config.toml keys, which can be any string; this one
// becomes a directory name and is handed to yt-dlp, which expands "$VAR" in
// output paths.
pub fn mood_download_dir(mood: &str) -> anyhow::Result<PathBuf> {
    let unsafe_name = mood.is_empty()
        || mood == "."
        || mood == ".."
        || mood.chars().any(|c| c == '/' || c == '$' || c.is_control());
    if unsafe_name {
        anyhow::bail!("mood name '{}' cannot be used as a download directory name", mood.escape_debug());
    }
    Ok(data_dir().join(mood))
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
    // symlink_metadata (not metadata) inspects the link itself rather than following
    // it, so a symlinked config.toml (common with dotfiles managed via Stow) is
    // detected here instead of being silently replaced by a plain file below.
    let is_symlink = std::fs::symlink_metadata(path)
        .map(|m| m.file_type().is_symlink())
        .unwrap_or(false);
    // Resolve through the symlink so the write and rename land on whatever the link
    // actually points to, leaving the symlink itself untouched.
    let target = if is_symlink { std::fs::canonicalize(path)? } else { path.to_path_buf() };
    let parent = target
        .parent()
        .ok_or_else(|| anyhow::anyhow!("config path {} has no parent directory", target.display()))?;
    std::fs::create_dir_all(parent)?;
    let text = toml::to_string_pretty(config)?;
    let file_name = target
        .file_name()
        .ok_or_else(|| anyhow::anyhow!("config path {} has no file name", target.display()))?;
    // Same directory as the target so the rename stays on one filesystem and is
    // atomic: a crash mid-write leaves the old config intact, never a truncated one.
    let tmp_path = parent.join(format!(".{}.tmp-{}", file_name.to_string_lossy(), std::process::id()));
    // Carry over the existing file's permission bits so a config with non-default
    // permissions (e.g. group-readable) doesn't change after a rewrite.
    let existing_permissions = std::fs::metadata(&target).ok().map(|m| m.permissions());
    let write_result = (|| -> anyhow::Result<()> {
        let mut file = std::fs::File::create(&tmp_path)?;
        file.write_all(text.as_bytes())?;
        file.sync_all()?;
        if let Some(perms) = existing_permissions {
            std::fs::set_permissions(&tmp_path, perms)?;
        }
        std::fs::rename(&tmp_path, &target)?;
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
    fn loop_playback_defaults_to_on_when_absent() {
        let cfg: Config = toml::from_str("default_mood = \"ambient\"\n[moods.ambient]\nsources = []\n").unwrap();
        assert!(cfg.loop_playback);
        let cfg: Config =
            toml::from_str("default_mood = \"ambient\"\nloop_playback = false\n[moods.ambient]\nsources = []\n").unwrap();
        assert!(!cfg.loop_playback);
    }

    #[test]
    fn audio_quality_defaults_to_min_when_absent() {
        let cfg: Config = toml::from_str("default_mood = \"ambient\"\n[moods.ambient]\nsources = []\n").unwrap();
        assert_eq!(cfg.audio_quality, "min");
        let cfg: Config = toml::from_str(default_config_toml()).unwrap();
        assert_eq!(cfg.audio_quality, "min");
    }

    #[test]
    fn ytdl_format_selectors_prefer_audio_only_and_fall_back_to_the_smallest_muxed_stream() {
        assert_eq!(ytdl_format_for("min"), "worstaudio/worst");
        assert_eq!(ytdl_format_for("max"), "bestaudio/worst");
        for selector in [ytdl_format_for("min"), ytdl_format_for("max")] {
            assert!(!selector.contains("video"), "{selector} could select a video stream first");
            assert!(selector.ends_with("/worst"), "{selector} could fall back to a large muxed stream");
        }
    }

    #[test]
    fn unknown_audio_quality_falls_back_to_the_default_selector() {
        assert_eq!(ytdl_format_for("medium"), ytdl_format_for(DEFAULT_AUDIO_QUALITY));
        assert!(is_valid_audio_quality("min") && is_valid_audio_quality("max"));
        assert!(!is_valid_audio_quality("MAX") && !is_valid_audio_quality(""));
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
    fn save_config_through_symlink_writes_to_real_target_and_leaves_symlink_intact() {
        let dir = tempfile::tempdir().unwrap();
        let real_target = dir.path().join("real-config.toml");
        std::fs::write(&real_target, "placeholder").unwrap();
        let symlink_path = dir.path().join("config.toml");
        std::os::unix::fs::symlink(&real_target, &symlink_path).unwrap();

        let cfg: Config = toml::from_str(default_config_toml()).unwrap();
        save_config(&symlink_path, &cfg).unwrap();

        let meta = std::fs::symlink_metadata(&symlink_path).unwrap();
        assert!(meta.file_type().is_symlink(), "save_config must not replace the symlink itself");
        let resolved = std::fs::read_link(&symlink_path).unwrap();
        assert_eq!(resolved, real_target);

        let contents = std::fs::read_to_string(&real_target).unwrap();
        assert!(contents.contains("default_mood"), "real target was not updated: {contents}");
    }

    #[test]
    fn save_config_preserves_existing_permission_bits() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        std::fs::write(&path, "placeholder").unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o640)).unwrap();

        let cfg: Config = toml::from_str(default_config_toml()).unwrap();
        save_config(&path, &cfg).unwrap();

        let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o640, "save_config must preserve pre-existing permission bits");
    }

    #[test]
    fn load_config_or_default_falls_back_when_file_is_missing() {
        let dir = tempfile::tempdir().unwrap();
        let cfg = load_config_or_default(&dir.path().join("missing.toml")).unwrap();
        assert_eq!(cfg.default_mood, "code-and-chill");
    }

    #[test]
    fn xdg_absolute_dir_rejects_empty_and_relative_values() {
        let _guard = crate::ENV_MUTEX.lock().unwrap_or_else(|p| p.into_inner());
        std::env::set_var("LOFI_TEST_XDG_DIR", "");
        assert_eq!(xdg_absolute_dir("LOFI_TEST_XDG_DIR"), None);
        std::env::set_var("LOFI_TEST_XDG_DIR", "relative/dir");
        assert_eq!(xdg_absolute_dir("LOFI_TEST_XDG_DIR"), None);
        std::env::set_var("LOFI_TEST_XDG_DIR", "/run/user/1000");
        assert_eq!(xdg_absolute_dir("LOFI_TEST_XDG_DIR"), Some(PathBuf::from("/run/user/1000")));
        std::env::remove_var("LOFI_TEST_XDG_DIR");
        assert_eq!(xdg_absolute_dir("LOFI_TEST_XDG_DIR"), None);
    }

    #[test]
    fn config_path_treats_empty_or_relative_xdg_config_home_as_unset() {
        let _guard = crate::ENV_MUTEX.lock().unwrap_or_else(|p| p.into_inner());
        let home = std::env::var_os("HOME").expect("HOME must be set for this test");
        let fallback = PathBuf::from(home).join(".config/lofi-launcher/config.toml");
        for bad in ["", "relative/config"] {
            std::env::set_var("XDG_CONFIG_HOME", bad);
            let path = config_path();
            std::env::remove_var("XDG_CONFIG_HOME");
            assert_eq!(path, fallback, "XDG_CONFIG_HOME={bad:?} was not ignored");
        }
    }

    #[test]
    fn data_dir_treats_empty_or_relative_xdg_data_home_as_unset() {
        let _guard = crate::ENV_MUTEX.lock().unwrap_or_else(|p| p.into_inner());
        let home = std::env::var_os("HOME").expect("HOME must be set for this test");
        let fallback = PathBuf::from(home).join(".local/share/lofi-launcher");
        for bad in ["", "relative/data"] {
            std::env::set_var("XDG_DATA_HOME", bad);
            let path = data_dir();
            std::env::remove_var("XDG_DATA_HOME");
            assert_eq!(path, fallback, "XDG_DATA_HOME={bad:?} was not ignored");
        }
        assert_eq!(data_dir(), fallback);
    }

    #[test]
    fn data_dir_respects_xdg_data_home() {
        let _guard = crate::ENV_MUTEX.lock().unwrap_or_else(|p| p.into_inner());
        std::env::set_var("XDG_DATA_HOME", "/tmp/lofi-test-xdg-data");
        let path = data_dir();
        let mood_dir = mood_download_dir("rainy-day");
        std::env::remove_var("XDG_DATA_HOME");
        assert_eq!(path, PathBuf::from("/tmp/lofi-test-xdg-data/lofi-launcher"));
        assert_eq!(mood_dir.unwrap(), PathBuf::from("/tmp/lofi-test-xdg-data/lofi-launcher/rainy-day"));
    }

    #[test]
    fn mood_download_dir_rejects_names_that_are_not_one_plain_path_component() {
        let _guard = crate::ENV_MUTEX.lock().unwrap_or_else(|p| p.into_inner());
        for bad in ["", ".", "..", "a/b", "../etc", "$HOME", "x\ny"] {
            assert!(mood_download_dir(bad).is_err(), "{bad:?} was accepted");
        }
        for good in BUILTIN_MOODS {
            assert!(mood_download_dir(good).unwrap().ends_with(good));
        }
    }

    #[test]
    fn config_path_respects_xdg_config_home() {
        let _guard = crate::ENV_MUTEX.lock().unwrap_or_else(|p| p.into_inner());
        std::env::set_var("XDG_CONFIG_HOME", "/tmp/lofi-test-xdg");
        let path = config_path();
        assert_eq!(path, std::path::PathBuf::from("/tmp/lofi-test-xdg/lofi-launcher/config.toml"));
        std::env::remove_var("XDG_CONFIG_HOME");
    }
}
