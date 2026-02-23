use anyhow::Context;
use clap::{Args, Parser, Subcommand};
use imessage_core::config::{AppConfig, AppPaths, WebhookConfigEntry, YamlConfig};
use std::path::Path;
use tracing::info;

/// Shared config flags used by both top-level CLI and bootstrap subcommand.
/// Uses exhaustive destructuring in methods so the compiler errors if a field
/// is added without updating `has_any()` or `to_yaml_config()`.
#[derive(Args, Debug, Clone, Default)]
pub struct ConfigFlags {
    /// Server password for API authentication
    #[arg(long)]
    pub password: Option<String>,

    /// HTTP server port
    #[arg(long, alias = "socket-port")]
    pub socket_port: Option<u16>,

    /// Server address (used in webhook NEW_SERVER events)
    #[arg(long, alias = "server-address")]
    pub server_address: Option<String>,

    /// Enable Private API (iMessage dylib injection)
    #[arg(long, alias = "enable-private-api")]
    pub enable_private_api: Option<bool>,

    /// Enable FaceTime Private API
    #[arg(long)]
    pub enable_facetime_private_api: Option<bool>,

    /// Enable FindMy Private API (device location decryption)
    #[arg(long)]
    pub enable_findmy_private_api: Option<bool>,

    /// Convert markdown to iMessage formatting
    #[arg(long, alias = "markdown-to-formatting")]
    pub markdown_to_formatting: Option<bool>,

    /// Webhook targets (repeatable). URL alone subscribes to all events;
    /// append ";event1,event2" to filter (e.g. "http://host;new-message,updated-message")
    #[arg(long)]
    pub webhook: Vec<String>,
}

impl ConfigFlags {
    /// Returns true if any config flag was set on the CLI.
    pub fn has_any(&self) -> bool {
        let Self {
            password,
            socket_port,
            server_address,
            enable_private_api,
            enable_facetime_private_api,
            enable_findmy_private_api,
            markdown_to_formatting,
            webhook,
        } = self;
        password.is_some()
            || socket_port.is_some()
            || server_address.is_some()
            || enable_private_api.is_some()
            || enable_facetime_private_api.is_some()
            || enable_findmy_private_api.is_some()
            || markdown_to_formatting.is_some()
            || !webhook.is_empty()
    }

    /// Convert CLI flags to a YamlConfig (for bootstrap serialization and merge).
    pub fn to_yaml_config(&self) -> YamlConfig {
        let Self {
            password,
            socket_port,
            server_address,
            enable_private_api,
            enable_facetime_private_api,
            enable_findmy_private_api,
            markdown_to_formatting,
            webhook,
        } = self;

        let webhooks = if webhook.is_empty() {
            None
        } else {
            Some(parse_webhook_args(webhook))
        };

        YamlConfig {
            password: password.clone(),
            socket_port: *socket_port,
            server_address: server_address.clone(),
            enable_private_api: *enable_private_api,
            enable_facetime_private_api: *enable_facetime_private_api,
            enable_findmy_private_api: *enable_findmy_private_api,
            markdown_to_formatting: *markdown_to_formatting,
            webhooks,
        }
    }
}

/// CLI arguments.
#[derive(Parser, Debug, Clone)]
#[command(name = "imessage-rs", version, about = "iMessage HTTP API bridge")]
pub struct CliArgs {
    #[command(subcommand)]
    pub command: Option<Command>,

    /// Path to config file (mutually exclusive with config flags).
    /// NOTE: when adding a field to ConfigFlags, also add it to conflicts_with_all below.
    #[arg(
        long,
        short = 'c',
        conflicts_with_all = [
            "password", "socket_port", "server_address",
            "enable_private_api", "enable_facetime_private_api",
            "enable_findmy_private_api", "markdown_to_formatting", "webhook"
        ]
    )]
    pub config: Option<String>,

    #[command(flatten)]
    pub flags: ConfigFlags,
}

#[derive(Subcommand, Debug, Clone)]
pub enum Command {
    /// Write configuration to config.yml from CLI flags
    Bootstrap {
        #[command(flatten)]
        flags: ConfigFlags,
    },
}

/// Parse webhook CLI args ("url" or "url;event1,event2") into WebhookConfigEntry values.
fn parse_webhook_args(args: &[String]) -> Vec<WebhookConfigEntry> {
    args.iter()
        .map(|arg| {
            if let Some((url, events_str)) = arg.split_once(';') {
                let events: Vec<String> = events_str
                    .split(',')
                    .map(|s| s.trim().to_string())
                    .filter(|s| !s.is_empty())
                    .collect();
                WebhookConfigEntry::Detailed {
                    url: url.to_string(),
                    events: Some(events),
                }
            } else {
                WebhookConfigEntry::Simple(arg.clone())
            }
        })
        .collect()
}

/// Load YAML config from a path (returns default if file doesn't exist).
pub fn load_yaml_config(path: &Path) -> YamlConfig {
    if !path.exists() {
        info!("No config file found at {}, using defaults", path.display());
        return YamlConfig::default();
    }

    match std::fs::read_to_string(path) {
        Ok(contents) => match serde_yaml::from_str(&contents) {
            Ok(cfg) => {
                info!("Loaded config from {}", path.display());
                cfg
            }
            Err(e) => {
                tracing::error!("Failed to parse {}: {e}", path.display());
                YamlConfig::default()
            }
        },
        Err(e) => {
            tracing::error!("Failed to read {}: {e}", path.display());
            YamlConfig::default()
        }
    }
}

/// Load YAML config from a path, returning an error if the file cannot be read or parsed.
fn load_yaml_config_strict(path: &Path) -> anyhow::Result<YamlConfig> {
    let contents = std::fs::read_to_string(path)
        .with_context(|| format!("Failed to read {}", path.display()))?;
    let cfg = serde_yaml::from_str(&contents)
        .with_context(|| format!("Failed to parse {}", path.display()))?;
    info!("Loaded config from {}", path.display());
    Ok(cfg)
}

/// Resolve final AppConfig from CLI args.
///
/// Three modes:
/// - `--config path`: load from custom path (errors if file missing)
/// - Any config flag set: use flags only, skip config file
/// - No flags: load default config.yml (silently falls back to defaults if missing)
pub fn resolve_config(cli: &CliArgs) -> anyhow::Result<AppConfig> {
    if let Some(ref config_path) = cli.config {
        let path = Path::new(config_path);
        if !path.exists() {
            anyhow::bail!("Config file not found: {}", path.display());
        }
        Ok(load_yaml_config_strict(path)?.into_app_config())
    } else if cli.flags.has_any() {
        Ok(cli.flags.to_yaml_config().into_app_config())
    } else {
        Ok(load_yaml_config(&AppPaths::yaml_config()).into_app_config())
    }
}

/// Run the bootstrap subcommand: write config.yml from CLI flags (destructive).
pub fn bootstrap_config(flags: ConfigFlags) -> anyhow::Result<()> {
    let data_dir = AppPaths::user_data();
    if !data_dir.exists() {
        std::fs::create_dir_all(&data_dir)?;
    }

    let yaml = flags.to_yaml_config();
    let path = AppPaths::yaml_config();
    let contents = serde_yaml::to_string(&yaml)?;
    std::fs::write(&path, contents)?;
    println!("Config written to {}", path.display());

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn default_cli() -> CliArgs {
        CliArgs {
            command: None,
            config: None,
            flags: ConfigFlags::default(),
        }
    }

    #[test]
    fn flags_has_any_detects_password() {
        let flags = ConfigFlags {
            password: Some("secret".to_string()),
            ..Default::default()
        };
        assert!(flags.has_any());
    }

    #[test]
    fn flags_has_any_detects_webhook() {
        let flags = ConfigFlags {
            webhook: vec!["http://localhost/hook".to_string()],
            ..Default::default()
        };
        assert!(flags.has_any());
    }

    #[test]
    fn flags_has_any_empty() {
        assert!(!ConfigFlags::default().has_any());
    }

    #[test]
    fn flags_to_yaml_config() {
        let flags = ConfigFlags {
            password: Some("pass".to_string()),
            socket_port: Some(8080),
            enable_private_api: Some(true),
            ..Default::default()
        };
        let yaml = flags.to_yaml_config();
        assert_eq!(yaml.password.as_deref(), Some("pass"));
        assert_eq!(yaml.socket_port, Some(8080));
        assert_eq!(yaml.enable_private_api, Some(true));
        assert!(yaml.server_address.is_none());
        assert!(yaml.webhooks.is_none());
    }

    #[test]
    fn flags_to_yaml_config_with_webhooks() {
        let flags = ConfigFlags {
            webhook: vec![
                "http://localhost:3000/hook".to_string(),
                "http://localhost:4000/hook;new-message,updated-message".to_string(),
            ],
            ..Default::default()
        };
        let yaml = flags.to_yaml_config();
        let wh = yaml.webhooks.unwrap();
        assert_eq!(wh.len(), 2);
        match &wh[0] {
            WebhookConfigEntry::Simple(url) => assert_eq!(url, "http://localhost:3000/hook"),
            _ => panic!("Expected Simple variant"),
        }
        match &wh[1] {
            WebhookConfigEntry::Detailed { url, events } => {
                assert_eq!(url, "http://localhost:4000/hook");
                assert_eq!(
                    events.as_ref().unwrap(),
                    &["new-message", "updated-message"]
                );
            }
            _ => panic!("Expected Detailed variant"),
        }
    }

    #[test]
    fn resolve_with_flags_skips_yaml() {
        // When flags are set, resolve_config should NOT load config file
        let cli = CliArgs {
            flags: ConfigFlags {
                password: Some("from-flags".to_string()),
                socket_port: Some(9999),
                ..Default::default()
            },
            ..default_cli()
        };
        let config = resolve_config(&cli).unwrap();
        assert_eq!(config.password, "from-flags");
        assert_eq!(config.socket_port, 9999);
        // Defaults apply for unset fields
        assert!(!config.enable_private_api);
    }

    #[test]
    fn resolve_no_flags_uses_defaults() {
        // No flags, no config file → all defaults
        let cli = default_cli();
        let config = resolve_config(&cli).unwrap();
        assert_eq!(config.socket_port, 1234);
        assert!(config.password.is_empty());
    }

    #[test]
    fn resolve_config_file_not_found() {
        let cli = CliArgs {
            config: Some("/tmp/nonexistent-imessage-rs-config.yml".to_string()),
            ..default_cli()
        };
        let result = resolve_config(&cli);
        assert!(result.is_err());
        assert!(
            result
                .unwrap_err()
                .to_string()
                .contains("Config file not found")
        );
    }

    #[test]
    fn resolve_explicit_config_parse_error() {
        let ts = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        let path = std::env::temp_dir().join(format!("imessage-rs-invalid-config-{ts}.yml"));
        std::fs::write(&path, "webhooks: [").unwrap();

        let cli = CliArgs {
            config: Some(path.to_string_lossy().to_string()),
            ..default_cli()
        };
        let result = resolve_config(&cli);

        let _ = std::fs::remove_file(&path);

        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("Failed to parse"));
    }

    #[test]
    fn yaml_into_app_config_defaults() {
        let yaml = YamlConfig {
            password: Some("yaml-pass".to_string()),
            socket_port: Some(5678),
            ..Default::default()
        };
        let config = yaml.into_app_config();
        assert_eq!(config.password, "yaml-pass");
        assert_eq!(config.socket_port, 5678);
        assert!(!config.enable_private_api); // default
    }

    #[test]
    fn parse_webhook_simple() {
        let yaml: YamlConfig = serde_yaml::from_str(
            r#"
webhooks:
  - "http://localhost:3000/hook"
"#,
        )
        .unwrap();
        assert_eq!(yaml.webhooks.as_ref().unwrap().len(), 1);
        match &yaml.webhooks.unwrap()[0] {
            WebhookConfigEntry::Simple(url) => assert_eq!(url, "http://localhost:3000/hook"),
            _ => panic!("Expected Simple variant"),
        }
    }

    #[test]
    fn parse_webhook_detailed() {
        let yaml: YamlConfig = serde_yaml::from_str(
            r#"
webhooks:
  - url: "http://localhost:3000/hook"
    events:
      - "new-message"
      - "updated-message"
"#,
        )
        .unwrap();
        match &yaml.webhooks.unwrap()[0] {
            WebhookConfigEntry::Detailed { url, events } => {
                assert_eq!(url, "http://localhost:3000/hook");
                assert_eq!(events.as_ref().unwrap().len(), 2);
            }
            _ => panic!("Expected Detailed variant"),
        }
    }
}
