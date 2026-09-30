//! The local, TUI-only config file. It never holds secrets.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Deserializer};
use uuid::Uuid;

use crate::density::Density;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum BusyBehavior {
    #[default]
    Queue,
    Interrupt,
}

impl BusyBehavior {
    pub fn as_str(self) -> &'static str {
        match self {
            BusyBehavior::Queue => "queue",
            BusyBehavior::Interrupt => "interrupt",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(default)]
pub struct WelcomeConfig {
    pub show: bool,
    pub art_file: Option<PathBuf>,
}

impl Default for WelcomeConfig {
    fn default() -> Self {
        WelcomeConfig {
            show: true,
            art_file: None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(default)]
pub struct LocalConfig {
    pub mouse: bool,
    pub busy_behavior: BusyBehavior,
    pub composer_max_lines: u16,
    pub welcome: WelcomeConfig,
    pub density: BTreeMap<String, Density>,
    /// The organization new chats go to, saved by `/organization`. An ID, not a secret.
    /// A value that is not a valid ID is ignored, so startup falls back to the default.
    #[serde(deserialize_with = "lenient_uuid")]
    pub organization: Option<Uuid>,
}

impl Default for LocalConfig {
    fn default() -> Self {
        LocalConfig {
            mouse: true,
            busy_behavior: BusyBehavior::Queue,
            composer_max_lines: 10,
            welcome: WelcomeConfig::default(),
            density: BTreeMap::new(),
            organization: None,
        }
    }
}

/// Reads an optional string and keeps it only when it parses as a UUID.
fn lenient_uuid<'de, D: Deserializer<'de>>(d: D) -> Result<Option<Uuid>, D::Error> {
    let text = Option::<String>::deserialize(d)?;
    Ok(text.and_then(|t| Uuid::parse_str(t.trim()).ok()))
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ConfigError {
    #[error("config key {0:?} looks like a secret; scuttle's config never holds secrets")]
    Secret(String),
    #[error("could not parse the config file: {0}")]
    Parse(String),
    #[error("could not read or write the config file: {0}")]
    Io(String),
}

fn looks_secret(key: &str) -> bool {
    let k = key.to_ascii_lowercase();
    ["token", "secret", "password", "api_key", "apikey"].contains(&k.as_str())
        || ["_token", "_secret", "_password", "_key"]
            .iter()
            .any(|s| k.ends_with(s))
}

fn check_secrets(table: &toml::Table, path: &str) -> Result<(), ConfigError> {
    check_secrets_impl(table, path, false)
}

fn check_secrets_impl(
    table: &toml::Table,
    path: &str,
    skip_secret_check: bool,
) -> Result<(), ConfigError> {
    for (key, value) in table {
        let full = if path.is_empty() {
            key.clone()
        } else {
            format!("{path}.{key}")
        };

        // Skip secret check if we're directly under the density table
        if !skip_secret_check && looks_secret(key) {
            return Err(ConfigError::Secret(full));
        }

        match value {
            toml::Value::Table(inner) => {
                // Check if we're at the root level and this is the density table
                let next_skip = path.is_empty() && key == "density";
                check_secrets_impl(inner, &full, next_skip)?;
            }
            toml::Value::Array(arr) => {
                check_array_secrets(arr, &full)?;
            }
            _ => {}
        }
    }
    Ok(())
}

fn check_array_secrets(arr: &[toml::Value], path: &str) -> Result<(), ConfigError> {
    for (idx, value) in arr.iter().enumerate() {
        match value {
            toml::Value::Table(table) => {
                let array_path = format!("{path}[{idx}]");
                check_secrets_impl(table, &array_path, false)?;
            }
            toml::Value::Array(inner_arr) => {
                let array_path = format!("{path}[{idx}]");
                check_array_secrets(inner_arr, &array_path)?;
            }
            _ => {}
        }
    }
    Ok(())
}

/// `$XDG_CONFIG_HOME/scuttle/config.toml`, else `$HOME/.config/scuttle/config.toml`.
pub fn config_path(env: &dyn Fn(&str) -> Option<String>) -> Option<PathBuf> {
    let base = env("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .or_else(|| env("HOME").map(|h| PathBuf::from(h).join(".config")))?;
    Some(base.join("scuttle").join("config.toml"))
}

pub fn load_from_str(text: &str) -> Result<LocalConfig, ConfigError> {
    let table: toml::Table = text
        .parse()
        .map_err(|e: toml::de::Error| ConfigError::Parse(e.to_string()))?;
    check_secrets(&table, "")?;
    toml::from_str(text).map_err(|e| ConfigError::Parse(e.to_string()))
}

pub fn load(path: &Path) -> Result<LocalConfig, ConfigError> {
    match std::fs::read_to_string(path) {
        Ok(text) => load_from_str(&text),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(LocalConfig::default()),
        Err(e) => Err(ConfigError::Io(e.to_string())),
    }
}

/// Sets `mouse` in the file, creating it if needed and preserving comments and formatting.
pub fn set_mouse(path: &Path, enabled: bool) -> Result<(), ConfigError> {
    let text = match std::fs::read_to_string(path) {
        Ok(t) => t,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(e) => return Err(ConfigError::Io(e.to_string())),
    };
    let mut doc: toml_edit::DocumentMut = text
        .parse()
        .map_err(|e: toml_edit::TomlError| ConfigError::Parse(e.to_string()))?;
    doc["mouse"] = toml_edit::value(enabled);
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).map_err(|e| ConfigError::Io(e.to_string()))?;
    }
    std::fs::write(path, doc.to_string()).map_err(|e| ConfigError::Io(e.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_the_saved_organization() {
        let id = uuid::Uuid::new_v4();
        let cfg = load_from_str(&format!("organization = \"{id}\"\n")).unwrap();
        assert_eq!(cfg.organization, Some(id));
        assert_eq!(LocalConfig::default().organization, None);
    }

    #[test]
    fn a_malformed_saved_organization_is_ignored() {
        let cfg = load_from_str(
            "organization = \"nope\"\nmouse = false\nbusy_behavior = \"interrupt\"\ncomposer_max_lines = 6\n",
        )
        .unwrap();
        assert_eq!(cfg.organization, None);
        assert!(!cfg.mouse);
        assert_eq!(cfg.busy_behavior, BusyBehavior::Interrupt);
        assert_eq!(cfg.composer_max_lines, 6);
        assert_eq!(
            load_from_str("organization = \"nope\"")
                .unwrap()
                .organization,
            None
        );
    }

    #[test]
    fn defaults_apply_to_an_empty_file() {
        assert_eq!(load_from_str("").unwrap(), LocalConfig::default());
        assert!(LocalConfig::default().mouse);
    }

    #[test]
    fn reads_every_supported_key() {
        let cfg = load_from_str(
            "mouse = false\nbusy_behavior = \"interrupt\"\ncomposer_max_lines = 6\n[welcome]\nshow = false\nart_file = \"/tmp/art.txt\"\n[density]\nread_file = \"hidden\"\n",
        )
        .unwrap();
        assert!(!cfg.mouse);
        assert_eq!(cfg.busy_behavior, BusyBehavior::Interrupt);
        assert_eq!(cfg.composer_max_lines, 6);
        assert!(!cfg.welcome.show);
        assert_eq!(cfg.density.get("read_file"), Some(&Density::Hidden));
    }

    #[test]
    fn rejects_secret_looking_keys_anywhere() {
        for text in [
            "token = \"x\"",
            "api_key = \"x\"",
            "[welcome]\nsession_token = \"x\"",
            "coder_password = \"x\"",
        ] {
            match load_from_str(text) {
                Err(ConfigError::Secret(key)) => assert!(!key.is_empty()),
                other => panic!("{text:?} gave {other:?}"),
            }
        }
    }

    #[test]
    fn config_path_prefers_xdg_then_home() {
        let xdg = |k: &str| match k {
            "XDG_CONFIG_HOME" => Some("/x".to_string()),
            "HOME" => Some("/h".to_string()),
            _ => None,
        };
        assert_eq!(
            config_path(&xdg),
            Some(PathBuf::from("/x/scuttle/config.toml"))
        );
        let home = |k: &str| (k == "HOME").then(|| "/h".to_string());
        assert_eq!(
            config_path(&home),
            Some(PathBuf::from("/h/.config/scuttle/config.toml"))
        );
    }

    #[test]
    fn set_mouse_preserves_comments_and_creates_the_file() {
        let dir = std::env::temp_dir().join(format!("scuttle-cfg-{}", uuid::Uuid::new_v4()));
        let path = dir.join("scuttle/config.toml");
        set_mouse(&path, false).unwrap();
        assert!(!load(&path).unwrap().mouse);
        std::fs::write(&path, "# keep me\nmouse = false\n").unwrap();
        set_mouse(&path, true).unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(text.contains("# keep me"));
        assert!(load(&path).unwrap().mouse);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn missing_file_loads_defaults() {
        let path =
            std::env::temp_dir().join(format!("scuttle-missing-{}.toml", uuid::Uuid::new_v4()));
        assert_eq!(load(&path).unwrap(), LocalConfig::default());
    }

    #[test]
    fn rejects_secrets_inside_arrays_of_tables() {
        // Test array of tables: [[hack]] with api_key inside
        match load_from_str("[[hack]]\napi_key = \"x\"\n") {
            Err(ConfigError::Secret(key)) => assert!(key.contains("api_key")),
            other => panic!("Expected Secret error, got {other:?}"),
        }

        // Test inline array: foo = [{ api_key = "x" }]
        match load_from_str("foo = [{ api_key = \"x\" }]\n") {
            Err(ConfigError::Secret(key)) => assert!(key.contains("api_key")),
            other => panic!("Expected Secret error, got {other:?}"),
        }
    }

    #[test]
    fn density_tool_names_are_not_treated_as_secrets() {
        let cfg =
            load_from_str("[density]\napi_key = \"hidden\"\nfetch_token = \"summary\"\n").unwrap();
        assert_eq!(cfg.density.get("api_key"), Some(&Density::Hidden));
        assert_eq!(cfg.density.get("fetch_token"), Some(&Density::Summary));
    }
}
