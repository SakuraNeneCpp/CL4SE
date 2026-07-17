use std::{
    fs,
    path::{Path, PathBuf},
};

use anyhow::{anyhow, Context, Result};
use directories::BaseDirs;
use serde::{Deserialize, Serialize};

const CONFIG_FILE_NAME: &str = "config.toml";
const CONFIG_DIRECTORY_NAME: &str = "cl4se";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Config {
    pub general: GeneralConfig,
    pub detection: DetectionConfig,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GeneralConfig {
    pub idle_action: IdleAction,
    pub shift_passthrough: bool,
    pub commit_key: CommitKeyConfig,
    pub log_level: LogLevel,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DetectionConfig {
    pub heuristic_timeout_secs: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum IdleAction {
    None,
    ShiftEnter,
    #[serde(rename = "capslock")]
    CapsLock,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CommitKeyConfig {
    Auto,
    Enter,
    CtrlM,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LogLevel {
    Error,
    Warn,
    Info,
    Debug,
    Trace,
}

impl Config {
    pub fn load() -> Result<Self> {
        Self::load_from(&Self::path()?)
    }

    pub fn path() -> Result<PathBuf> {
        Ok(config_directory()?.join(CONFIG_FILE_NAME))
    }

    pub fn save(&self) -> Result<PathBuf> {
        let path = Self::path()?;
        Self::save_to(&path, self)?;
        Ok(path)
    }

    fn load_from(path: &Path) -> Result<Self> {
        if path.exists() {
            let contents = fs::read_to_string(path)
                .with_context(|| format!("failed to read config file: {}", path.display()))?;
            return toml::from_str(&contents)
                .with_context(|| format!("failed to parse config file: {}", path.display()));
        }

        let config = Self::default();
        Self::save_to(path, &config)?;
        Ok(config)
    }

    fn save_to(path: &Path, config: &Self) -> Result<()> {
        let parent = path.parent().ok_or_else(|| {
            anyhow!(
                "configuration path has no parent directory: {}",
                path.display()
            )
        })?;
        fs::create_dir_all(parent).with_context(|| {
            format!(
                "failed to create configuration directory: {}",
                parent.display()
            )
        })?;
        let contents = toml::to_string_pretty(config).context("failed to serialize config")?;
        fs::write(path, contents)
            .with_context(|| format!("failed to write config file: {}", path.display()))
    }
}

pub(crate) fn config_directory() -> Result<PathBuf> {
    let base_dirs = BaseDirs::new()
        .ok_or_else(|| anyhow!("could not determine the configuration directory"))?;
    Ok(base_dirs.config_dir().join(CONFIG_DIRECTORY_NAME))
}

impl IdleAction {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::None => "none",
            Self::ShiftEnter => "shift_enter",
            Self::CapsLock => "capslock",
        }
    }
}

impl Default for Config {
    fn default() -> Self {
        Self {
            general: GeneralConfig {
                idle_action: IdleAction::None,
                shift_passthrough: true,
                commit_key: CommitKeyConfig::Auto,
                log_level: LogLevel::Info,
            },
            detection: DetectionConfig {
                heuristic_timeout_secs: 30,
            },
        }
    }
}

impl LogLevel {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Error => "error",
            Self::Warn => "warn",
            Self::Info => "info",
            Self::Debug => "debug",
            Self::Trace => "trace",
        }
    }
}

#[cfg(test)]
mod tests {
    use std::time::{SystemTime, UNIX_EPOCH};

    use super::*;

    #[test]
    fn defaults_match_readme() {
        let config = Config::default();

        assert_eq!(config.general.idle_action, IdleAction::None);
        assert!(config.general.shift_passthrough);
        assert_eq!(config.general.commit_key, CommitKeyConfig::Auto);
        assert_eq!(config.general.log_level, LogLevel::Info);
        assert_eq!(config.detection.heuristic_timeout_secs, 30);
    }

    #[test]
    fn missing_file_is_created_with_defaults() -> Result<()> {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .context("system clock is before the Unix epoch")?
            .as_nanos();
        let directory =
            std::env::temp_dir().join(format!("cl4se-config-test-{}-{unique}", std::process::id()));
        let path = directory.join(CONFIG_FILE_NAME);

        let loaded = Config::load_from(&path)?;
        let written = fs::read_to_string(&path)?;
        let parsed: Config = toml::from_str(&written)?;
        fs::remove_dir_all(&directory)?;

        assert!(written.contains("idle_action = \"none\""));
        assert!(written.contains("shift_passthrough = true"));
        assert!(written.contains("commit_key = \"auto\""));
        assert!(written.contains("log_level = \"info\""));
        assert!(written.contains("heuristic_timeout_secs = 30"));
        assert_eq!(loaded, Config::default());
        assert_eq!(parsed, Config::default());
        Ok(())
    }

    #[test]
    fn supported_non_default_values_deserialize() -> Result<()> {
        let config: Config = toml::from_str(
            r#"
[general]
idle_action = "capslock"
shift_passthrough = false
commit_key = "ctrl_m"
log_level = "trace"

[detection]
heuristic_timeout_secs = 5
"#,
        )?;

        assert_eq!(config.general.idle_action, IdleAction::CapsLock);
        assert!(!config.general.shift_passthrough);
        assert_eq!(config.general.commit_key, CommitKeyConfig::CtrlM);
        assert_eq!(config.general.log_level, LogLevel::Trace);
        assert_eq!(config.detection.heuristic_timeout_secs, 5);
        Ok(())
    }

    #[test]
    fn shift_enter_idle_action_deserializes_without_changing_the_default() -> Result<()> {
        let config: Config = toml::from_str(
            r#"
[general]
idle_action = "shift_enter"
shift_passthrough = true
commit_key = "auto"
log_level = "info"

[detection]
heuristic_timeout_secs = 30
"#,
        )?;

        assert_eq!(config.general.idle_action, IdleAction::ShiftEnter);
        assert_eq!(Config::default().general.idle_action, IdleAction::None);
        Ok(())
    }

    #[test]
    fn changed_config_is_persisted_and_reloaded() -> Result<()> {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .context("system clock is before the Unix epoch")?
            .as_nanos();
        let directory =
            std::env::temp_dir().join(format!("cl4se-save-test-{}-{unique}", std::process::id()));
        let path = directory.join(CONFIG_FILE_NAME);
        let mut config = Config::default();
        config.general.idle_action = IdleAction::ShiftEnter;

        Config::save_to(&path, &config)?;
        let reloaded = Config::load_from(&path)?;
        fs::remove_dir_all(directory)?;

        assert_eq!(reloaded, config);
        Ok(())
    }
}
