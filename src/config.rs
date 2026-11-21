use std::collections::HashMap;
use std::fs;
use std::io::ErrorKind;
use std::path::PathBuf;

use directories::BaseDirs;
use serde::Deserialize;
use thiserror::Error;

use crate::keymap::{Keymap, KeymapError};

pub const CLIENT_ID_ENV_VAR: &str = "VIOLIN_CLIENT_ID";
const APP_DIR_NAME: &str = "violin";
const CONFIG_FILE_NAME: &str = "config.toml";
const TOKENS_FILE_NAME: &str = "tokens.json";
const SOLOIST_DIR_NAME: &str = "soloist";

#[derive(Debug, Error)]
pub enum ConfigError {
    #[error("could not determine Linux XDG base directories")]
    MissingBaseDirs,
    #[error("failed to read config file {}: {source}", path.display())]
    ReadFile {
        path: PathBuf,
        source: std::io::Error,
    },
    #[error("failed to parse config file {}: {source}", path.display())]
    ParseToml {
        path: PathBuf,
        source: toml::de::Error,
    },
    #[error("invalid keybindings in {}: {source}", path.display())]
    InvalidKeybindings { path: PathBuf, source: KeymapError },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AppPaths {
    pub config_file: PathBuf,
    pub tokens_file: PathBuf,
    pub soloist_dir: PathBuf,
}

impl AppPaths {
    /// Resolves XDG configuration and data paths for `violin`.
    pub fn resolve() -> Result<Self, ConfigError> {
        let base_dirs = BaseDirs::new().ok_or(ConfigError::MissingBaseDirs)?;
        Ok(Self {
            config_file: base_dirs
                .config_dir()
                .join(APP_DIR_NAME)
                .join(CONFIG_FILE_NAME),
            tokens_file: base_dirs
                .data_dir()
                .join(APP_DIR_NAME)
                .join(TOKENS_FILE_NAME),
            soloist_dir: base_dirs
                .data_dir()
                .join(APP_DIR_NAME)
                .join(SOLOIST_DIR_NAME),
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Config {
    pub client_id: String,
    pub paths: AppPaths,
    pub keymap: Keymap,
}

impl Config {
    /// Loads configuration from `$XDG_CONFIG_HOME/violin/config.toml` with `VIOLIN_CLIENT_ID` override.
    pub fn load() -> Result<Self, ConfigError> {
        let paths = AppPaths::resolve()?;
        let parsed = match fs::read_to_string(&paths.config_file) {
            Ok(raw) => parse_raw_config(&raw).map_err(|source| ConfigError::ParseToml {
                path: paths.config_file.clone(),
                source,
            })?,
            Err(err) if err.kind() == ErrorKind::NotFound => ParsedConfig::default(),
            Err(source) => {
                return Err(ConfigError::ReadFile {
                    path: paths.config_file.clone(),
                    source,
                });
            }
        };

        let keymap = Keymap::from_overrides(&parsed.keybindings).map_err(|source| {
            ConfigError::InvalidKeybindings {
                path: paths.config_file.clone(),
                source,
            }
        })?;
        let env_override = std::env::var(CLIENT_ID_ENV_VAR).ok();
        let client_id = resolve_client_id(env_override, parsed.client_id);
        Ok(Self {
            client_id,
            paths,
            keymap,
        })
    }
}

#[derive(Debug, Deserialize)]
#[serde(untagged)]
enum StringOrVec {
    One(String),
    Many(Vec<String>),
}

#[derive(Debug, Deserialize, Default)]
struct RawConfig {
    client_id: Option<String>,
    #[serde(default)]
    keybindings: HashMap<String, StringOrVec>,
}

#[derive(Debug, Default)]
struct ParsedConfig {
    client_id: Option<String>,
    keybindings: HashMap<String, Vec<String>>,
}

fn parse_raw_config(raw: &str) -> Result<ParsedConfig, toml::de::Error> {
    let parsed: RawConfig = toml::from_str(raw)?;
    let keybindings = parsed
        .keybindings
        .into_iter()
        .map(|(action, value)| {
            let keys = match value {
                StringOrVec::One(single) => vec![single],
                StringOrVec::Many(many) => many,
            };
            (action, keys)
        })
        .collect();
    Ok(ParsedConfig {
        client_id: parsed.client_id,
        keybindings,
    })
}

/// Parses a `config.toml` payload and extracts `client_id` if present.
pub fn parse_config_toml(raw: &str) -> Result<Option<String>, toml::de::Error> {
    Ok(parse_raw_config(raw)?.client_id)
}

/// Parses a `config.toml` payload and extracts the `[keybindings]` table as action-to-keys lists.
pub fn parse_keybindings_toml(raw: &str) -> Result<HashMap<String, Vec<String>>, toml::de::Error> {
    Ok(parse_raw_config(raw)?.keybindings)
}

/// Resolves the active `client_id` from env override, config file, or the built-in Spotify public client ID.
pub fn resolve_client_id(env_override: Option<String>, file_value: Option<String>) -> String {
    env_override
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .or_else(|| {
            file_value
                .as_deref()
                .map(str::trim)
                .filter(|value| !value.is_empty())
        })
        .map(ToOwned::to_owned)
        .unwrap_or_else(|| crate::auth::DEFAULT_SPOTIFY_CLIENT_ID.to_string())
}

#[cfg(test)]
mod tests {
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

    use crate::app::{Action, InputMode};

    use super::*;

    #[test]
    fn parse_config_toml_extracts_client_id() {
        let parsed = parse_config_toml("client_id = \"abc123\"\n").unwrap();
        assert_eq!(parsed.as_deref(), Some("abc123"));

        let empty = parse_config_toml("").unwrap();
        assert_eq!(empty, None);
    }

    #[test]
    fn parse_config_toml_rejects_malformed_toml() {
        let err = parse_config_toml("client_id = [unterminated").unwrap_err();
        assert!(!err.to_string().is_empty());
    }

    #[test]
    fn parse_keybindings_toml_accepts_single_and_array_values() {
        let raw = r#"
            [keybindings]
            search = "ctrl+f"
            move_down = ["j", "pagedown", "down"]
        "#;
        let parsed = parse_keybindings_toml(raw).unwrap();
        assert_eq!(parsed.get("search"), Some(&vec!["ctrl+f".to_string()]));
        assert_eq!(
            parsed.get("move_down"),
            Some(&vec![
                "j".to_string(),
                "pagedown".to_string(),
                "down".to_string()
            ])
        );

        let keymap = Keymap::from_overrides(&parsed).unwrap();
        assert_eq!(
            keymap.map_key(
                InputMode::Normal,
                KeyEvent::new(KeyCode::Char('f'), KeyModifiers::CONTROL)
            ),
            Some(Action::EnterSearchInput)
        );
        assert_eq!(
            keymap.map_key(
                InputMode::Normal,
                KeyEvent::new(KeyCode::PageDown, KeyModifiers::NONE)
            ),
            Some(Action::MoveDown)
        );
    }

    #[test]
    fn parse_keybindings_toml_rejects_invalid_toml_types() {
        let raw = r#"
            [keybindings]
            quit = 42
        "#;
        assert!(parse_keybindings_toml(raw).is_err());
    }

    #[test]
    fn resolve_client_id_prefers_non_empty_env_override_and_trims() {
        let resolved =
            resolve_client_id(Some("  env_id \n".to_string()), Some("file_id".to_string()));
        assert_eq!(resolved, "env_id");

        let fallback = resolve_client_id(Some("   ".to_string()), Some("  file_id  ".to_string()));
        assert_eq!(fallback, "file_id");
    }

    #[test]
    fn resolve_client_id_falls_back_to_default_when_missing_or_blank() {
        assert_eq!(
            resolve_client_id(None, Some("   \t ".to_string())),
            crate::auth::DEFAULT_SPOTIFY_CLIENT_ID
        );
        assert_eq!(
            resolve_client_id(None, None),
            crate::auth::DEFAULT_SPOTIFY_CLIENT_ID
        );
    }
}
