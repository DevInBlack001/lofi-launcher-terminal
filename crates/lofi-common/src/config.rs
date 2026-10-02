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
    fn config_path_respects_xdg_config_home() {
        let _guard = crate::ENV_MUTEX.lock().unwrap_or_else(|p| p.into_inner());
        std::env::set_var("XDG_CONFIG_HOME", "/tmp/lofi-test-xdg");
        let path = config_path();
        assert_eq!(path, std::path::PathBuf::from("/tmp/lofi-test-xdg/lofi-launcher/config.toml"));
        std::env::remove_var("XDG_CONFIG_HOME");
    }
}
