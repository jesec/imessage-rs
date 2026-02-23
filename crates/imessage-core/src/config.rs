use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use tracing::info;

/// YAML config file structure (config.yml in the data directory).
#[derive(Debug, Deserialize, Serialize, Default, Clone)]
pub struct YamlConfig {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub password: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub socket_port: Option<u16>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub server_address: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub enable_private_api: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub enable_facetime_private_api: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub enable_findmy_private_api: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub markdown_to_formatting: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub webhooks: Option<Vec<WebhookConfigEntry>>,
}

/// A webhook entry in YAML config.
/// Can be a simple URL string or an object with url + events.
#[derive(Debug, Deserialize, Serialize, Clone)]
#[serde(untagged)]
pub enum WebhookConfigEntry {
    Simple(String),
    Detailed {
        url: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        events: Option<Vec<String>>,
    },
}

/// Merged configuration (YAML + CLI, with defaults applied).
#[derive(Debug, Clone)]
pub struct AppConfig {
    pub password: String,
    pub socket_port: u16,
    pub server_address: String,
    pub enable_private_api: bool,
    pub enable_facetime_private_api: bool,
    pub enable_findmy_private_api: bool,
    pub markdown_to_formatting: bool,
    pub webhooks: Vec<WebhookConfigEntry>,
}

impl Default for AppConfig {
    fn default() -> Self {
        Self {
            password: String::new(),
            socket_port: 1234,
            server_address: String::new(),
            enable_private_api: false,
            enable_facetime_private_api: false,
            enable_findmy_private_api: false,
            markdown_to_formatting: false,
            webhooks: vec![],
        }
    }
}

impl YamlConfig {
    /// Convert to final AppConfig by applying defaults.
    /// Uses exhaustive destructuring so the compiler errors if a field is added
    /// to YamlConfig without updating this method.
    pub fn into_app_config(self) -> AppConfig {
        let defaults = AppConfig::default();
        let Self {
            password,
            socket_port,
            server_address,
            enable_private_api,
            enable_facetime_private_api,
            enable_findmy_private_api,
            markdown_to_formatting,
            webhooks,
        } = self;
        AppConfig {
            password: password.unwrap_or(defaults.password),
            socket_port: socket_port.unwrap_or(defaults.socket_port),
            server_address: server_address.unwrap_or(defaults.server_address),
            enable_private_api: enable_private_api.unwrap_or(defaults.enable_private_api),
            enable_facetime_private_api: enable_facetime_private_api
                .unwrap_or(defaults.enable_facetime_private_api),
            enable_findmy_private_api: enable_findmy_private_api
                .unwrap_or(defaults.enable_findmy_private_api),
            markdown_to_formatting: markdown_to_formatting
                .unwrap_or(defaults.markdown_to_formatting),
            webhooks: webhooks.unwrap_or(defaults.webhooks),
        }
    }
}

/// Paths used by the application.
pub struct AppPaths;

impl AppPaths {
    /// User data directory: ~/Library/Application Support/imessage-rs
    pub fn user_data() -> PathBuf {
        home::home_dir()
            .unwrap_or_else(|| PathBuf::from("/tmp"))
            .join("Library")
            .join("Application Support")
            .join("imessage-rs")
    }

    /// PID file location
    pub fn pid_file() -> PathBuf {
        Self::user_data().join(".imessage-rs.pid")
    }

    /// iMessage database path
    pub fn imessage_db() -> PathBuf {
        home::home_dir()
            .unwrap_or_else(|| PathBuf::from("/tmp"))
            .join("Library")
            .join("Messages")
            .join("chat.db")
    }

    /// iMessage database WAL path
    pub fn imessage_db_wal() -> PathBuf {
        Self::imessage_db().with_extension("db-wal")
    }

    /// Attachments directory (app data)
    pub fn attachments_dir() -> PathBuf {
        Self::user_data().join("Attachments")
    }

    /// Messages attachments directory (inside ~/Library/Messages/).
    /// Used for Private API sends and on macOS Monterey+ where AppleScript
    /// requires files to already be inside the Messages sandbox.
    pub fn messages_attachments_dir() -> PathBuf {
        home::home_dir()
            .unwrap_or_else(|| PathBuf::from("/tmp"))
            .join("Library")
            .join("Messages")
            .join("Attachments")
            .join("imessage-rs")
    }

    /// Cached attachments directory
    pub fn attachment_cache_dir() -> PathBuf {
        Self::attachments_dir().join("Cached")
    }

    /// Convert staging directory
    pub fn convert_dir() -> PathBuf {
        Self::user_data().join("Convert")
    }

    /// YAML config file path
    pub fn yaml_config() -> PathBuf {
        Self::user_data().join("config.yml")
    }

    /// Server version (from Cargo package version)
    pub fn version() -> &'static str {
        env!("CARGO_PKG_VERSION")
    }
}

/// Ensure all required directories exist.
pub fn setup_directories() -> anyhow::Result<()> {
    let dirs = [
        AppPaths::user_data(),
        AppPaths::attachments_dir(),
        AppPaths::attachment_cache_dir(),
        AppPaths::convert_dir(),
    ];

    for dir in &dirs {
        if !dir.exists() {
            std::fs::create_dir_all(dir)?;
            info!("Created directory: {}", dir.display());
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_config_values() {
        let cfg = AppConfig::default();
        assert_eq!(cfg.socket_port, 1234);
        assert!(!cfg.enable_private_api);
    }

    #[test]
    fn user_data_path_contains_imessage_rs() {
        let path = AppPaths::user_data();
        assert!(path.to_str().unwrap().contains("imessage-rs"));
    }
}
