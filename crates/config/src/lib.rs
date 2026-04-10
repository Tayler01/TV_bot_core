//! Runtime configuration loading and validation.

use std::{
    collections::HashMap,
    fs,
    path::{Path, PathBuf},
};

use secrecy::SecretString;
use serde::Deserialize;
use thiserror::Error;
use tv_bot_core_types::{BrokerEnvironment, RuntimeMode};

const DEFAULT_HTTP_BIND: &str = "127.0.0.1:8080";
const DEFAULT_WEBSOCKET_BIND: &str = "127.0.0.1:8081";
const DEFAULT_SQLITE_PATH: &str = "data/tv_bot_core.sqlite";
const DEFAULT_LOG_LEVEL: &str = "info";

#[derive(Clone, Debug)]
pub struct AppConfig {
    pub runtime: RuntimeConfig,
    pub market_data: MarketDataConfig,
    pub broker: BrokerConfig,
    pub persistence: PersistenceConfig,
    pub control_api: ControlApiConfig,
    pub logging: LoggingConfig,
}

#[derive(Clone, Debug)]
pub struct RuntimeConfig {
    pub startup_mode: RuntimeMode,
    pub default_strategy_path: Option<PathBuf>,
    pub allow_sqlite_fallback: bool,
}

#[derive(Clone, Debug)]
pub struct MarketDataConfig {
    pub dataset: Option<String>,
    pub gateway: Option<String>,
    pub api_key: Option<SecretString>,
}

#[derive(Clone, Debug)]
pub struct BrokerConfig {
    pub environment: Option<BrokerEnvironment>,
    pub http_base_url: Option<String>,
    pub websocket_url: Option<String>,
    pub username: Option<String>,
    pub password: Option<SecretString>,
    pub cid: Option<String>,
    pub sec: Option<SecretString>,
    pub app_id: Option<String>,
    pub app_version: Option<String>,
    pub device_id: Option<String>,
    pub paper_account_name: Option<String>,
    pub live_account_name: Option<String>,
}

#[derive(Clone, Debug)]
pub struct PersistenceConfig {
    pub primary_url: Option<String>,
    pub sqlite_fallback: SqliteFallbackConfig,
}

#[derive(Clone, Debug)]
pub struct SqliteFallbackConfig {
    pub enabled: bool,
    pub path: PathBuf,
}

#[derive(Clone, Debug)]
pub struct ControlApiConfig {
    pub http_bind: String,
    pub websocket_bind: String,
}

#[derive(Clone, Debug)]
pub struct LoggingConfig {
    pub level: String,
    pub json: bool,
}

pub trait Environment {
    fn get(&self, key: &str) -> Option<String>;
}

#[derive(Debug, Default)]
pub struct StdEnvironment;

impl Environment for StdEnvironment {
    fn get(&self, key: &str) -> Option<String> {
        std::env::var(key).ok()
    }
}

#[derive(Debug, Default)]
pub struct MapEnvironment {
    values: HashMap<String, String>,
}

impl MapEnvironment {
    pub fn new(values: HashMap<String, String>) -> Self {
        Self { values }
    }
}

impl Environment for MapEnvironment {
    fn get(&self, key: &str) -> Option<String> {
        self.values.get(key).cloned()
    }
}

#[derive(Debug, Error)]
pub enum ConfigError {
    #[error("failed to read config file `{path}`: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("failed to parse config file `{path}`: {source}")]
    TomlDeserialize {
        path: PathBuf,
        #[source]
        source: toml::de::Error,
    },
    #[error("missing required config field `{0}`")]
    MissingRequiredField(&'static str),
    #[error("invalid environment value for `{key}`: `{value}` ({message})")]
    InvalidEnvironmentValue {
        key: String,
        value: String,
        message: String,
    },
}

impl AppConfig {
    pub fn load(path: Option<&Path>, env: &impl Environment) -> Result<Self, ConfigError> {
        let mut partial = if let Some(path) = path {
            let body = fs::read_to_string(path).map_err(|source| ConfigError::Io {
                path: path.to_path_buf(),
                source,
            })?;
            Self::partial_from_toml(path, &body)?
        } else {
            PartialAppConfig::default()
        };

        apply_env_overrides(&mut partial, env)?;
        partial.build()
    }

    pub fn from_toml_str(
        source_path: impl Into<PathBuf>,
        body: &str,
        env: &impl Environment,
    ) -> Result<Self, ConfigError> {
        let path = source_path.into();
        let mut partial = Self::partial_from_toml(&path, body)?;
        apply_env_overrides(&mut partial, env)?;
        partial.build()
    }

    fn partial_from_toml(path: &Path, body: &str) -> Result<PartialAppConfig, ConfigError> {
        toml::from_str::<PartialAppConfig>(body).map_err(|source| ConfigError::TomlDeserialize {
            path: path.to_path_buf(),
            source,
        })
    }
}

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct PartialAppConfig {
    runtime: Option<PartialRuntimeConfig>,
    market_data: Option<PartialMarketDataConfig>,
    broker: Option<PartialBrokerConfig>,
    persistence: Option<PartialPersistenceConfig>,
    control_api: Option<PartialControlApiConfig>,
    logging: Option<PartialLoggingConfig>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct PartialRuntimeConfig {
    startup_mode: Option<RuntimeMode>,
    default_strategy_path: Option<PathBuf>,
    allow_sqlite_fallback: Option<bool>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct PartialMarketDataConfig {
    dataset: Option<String>,
    gateway: Option<String>,
    api_key: Option<String>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct PartialBrokerConfig {
    environment: Option<BrokerEnvironment>,
    http_base_url: Option<String>,
    websocket_url: Option<String>,
    username: Option<String>,
    password: Option<String>,
    cid: Option<String>,
    sec: Option<String>,
    app_id: Option<String>,
    app_version: Option<String>,
    device_id: Option<String>,
    paper_account_name: Option<String>,
    live_account_name: Option<String>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct PartialPersistenceConfig {
    primary_url: Option<String>,
    sqlite_fallback: Option<PartialSqliteFallbackConfig>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct PartialSqliteFallbackConfig {
    enabled: Option<bool>,
    path: Option<PathBuf>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct PartialControlApiConfig {
    http_bind: Option<String>,
    websocket_bind: Option<String>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct PartialLoggingConfig {
    level: Option<String>,
    json: Option<bool>,
}

impl PartialAppConfig {
    fn runtime_mut(&mut self) -> &mut PartialRuntimeConfig {
        self.runtime
            .get_or_insert_with(PartialRuntimeConfig::default)
    }

    fn market_data_mut(&mut self) -> &mut PartialMarketDataConfig {
        self.market_data
            .get_or_insert_with(PartialMarketDataConfig::default)
    }

    fn broker_mut(&mut self) -> &mut PartialBrokerConfig {
        self.broker.get_or_insert_with(PartialBrokerConfig::default)
    }

    fn persistence_mut(&mut self) -> &mut PartialPersistenceConfig {
        self.persistence
            .get_or_insert_with(PartialPersistenceConfig::default)
    }

    fn control_api_mut(&mut self) -> &mut PartialControlApiConfig {
        self.control_api
            .get_or_insert_with(PartialControlApiConfig::default)
    }

    fn logging_mut(&mut self) -> &mut PartialLoggingConfig {
        self.logging
            .get_or_insert_with(PartialLoggingConfig::default)
    }

    fn build(self) -> Result<AppConfig, ConfigError> {
        let runtime = self.runtime.unwrap_or_default();
        let market_data = self.market_data.unwrap_or_default();
        let broker = self.broker.unwrap_or_default();
        let persistence = self.persistence.unwrap_or_default();
        let sqlite_fallback = persistence.sqlite_fallback.unwrap_or_default();
        let control_api = self.control_api.unwrap_or_default();
        let logging = self.logging.unwrap_or_default();

        let startup_mode = runtime
            .startup_mode
            .ok_or(ConfigError::MissingRequiredField("runtime.startup_mode"))?;

        Ok(AppConfig {
            runtime: RuntimeConfig {
                startup_mode,
                default_strategy_path: runtime.default_strategy_path,
                allow_sqlite_fallback: runtime.allow_sqlite_fallback.unwrap_or(false),
            },
            market_data: MarketDataConfig {
                dataset: market_data.dataset,
                gateway: market_data.gateway,
                api_key: market_data
                    .api_key
                    .map(|value| SecretString::new(value.into())),
            },
            broker: BrokerConfig {
                environment: broker.environment,
                http_base_url: broker.http_base_url,
                websocket_url: broker.websocket_url,
                username: broker.username,
                password: broker.password.map(|value| SecretString::new(value.into())),
                cid: broker.cid,
                sec: broker.sec.map(|value| SecretString::new(value.into())),
                app_id: broker.app_id,
                app_version: broker.app_version,
                device_id: broker.device_id,
                paper_account_name: broker.paper_account_name,
                live_account_name: broker.live_account_name,
            },
            persistence: PersistenceConfig {
                primary_url: persistence.primary_url,
                sqlite_fallback: SqliteFallbackConfig {
                    enabled: sqlite_fallback.enabled.unwrap_or(false),
                    path: sqlite_fallback
                        .path
                        .unwrap_or_else(|| PathBuf::from(DEFAULT_SQLITE_PATH)),
                },
            },
            control_api: ControlApiConfig {
                http_bind: control_api
                    .http_bind
                    .unwrap_or_else(|| DEFAULT_HTTP_BIND.to_owned()),
                websocket_bind: control_api
                    .websocket_bind
                    .unwrap_or_else(|| DEFAULT_WEBSOCKET_BIND.to_owned()),
            },
            logging: LoggingConfig {
                level: logging
                    .level
                    .unwrap_or_else(|| DEFAULT_LOG_LEVEL.to_owned()),
                json: logging.json.unwrap_or(false),
            },
        })
    }
}

fn apply_env_overrides(
    partial: &mut PartialAppConfig,
    env: &impl Environment,
) -> Result<(), ConfigError> {
    if let Some(value) = env.get("TV_BOT__RUNTIME__STARTUP_MODE") {
        partial.runtime_mut().startup_mode =
            Some(parse_runtime_mode("TV_BOT__RUNTIME__STARTUP_MODE", &value)?);
    }

    if let Some(value) = env.get("TV_BOT__RUNTIME__DEFAULT_STRATEGY_PATH") {
        partial.runtime_mut().default_strategy_path = Some(PathBuf::from(value));
    }

    if let Some(value) = env.get("TV_BOT__RUNTIME__ALLOW_SQLITE_FALLBACK") {
        partial.runtime_mut().allow_sqlite_fallback = Some(parse_bool(
            "TV_BOT__RUNTIME__ALLOW_SQLITE_FALLBACK",
            &value,
        )?);
    }

    apply_string_override(env, "TV_BOT__MARKET_DATA__DATASET", |value| {
        partial.market_data_mut().dataset = Some(value);
    });
    apply_string_override(env, "TV_BOT__MARKET_DATA__GATEWAY", |value| {
        partial.market_data_mut().gateway = Some(value);
    });
    apply_string_override(env, "TV_BOT__MARKET_DATA__API_KEY", |value| {
        partial.market_data_mut().api_key = Some(value);
    });

    apply_string_override(env, "TV_BOT__BROKER__HTTP_BASE_URL", |value| {
        partial.broker_mut().http_base_url = Some(value);
    });
    if let Some(value) = env.get("TV_BOT__BROKER__ENVIRONMENT") {
        partial.broker_mut().environment = Some(parse_broker_environment(
            "TV_BOT__BROKER__ENVIRONMENT",
            &value,
        )?);
    }
    apply_string_override(env, "TV_BOT__BROKER__WEBSOCKET_URL", |value| {
        partial.broker_mut().websocket_url = Some(value);
    });
    apply_string_override(env, "TV_BOT__BROKER__USERNAME", |value| {
        partial.broker_mut().username = Some(value);
    });
    apply_string_override(env, "TV_BOT__BROKER__PASSWORD", |value| {
        partial.broker_mut().password = Some(value);
    });
    apply_string_override(env, "TV_BOT__BROKER__CID", |value| {
        partial.broker_mut().cid = Some(value);
    });
    apply_string_override(env, "TV_BOT__BROKER__SEC", |value| {
        partial.broker_mut().sec = Some(value);
    });
    apply_string_override(env, "TV_BOT__BROKER__APP_ID", |value| {
        partial.broker_mut().app_id = Some(value);
    });
    apply_string_override(env, "TV_BOT__BROKER__APP_VERSION", |value| {
        partial.broker_mut().app_version = Some(value);
    });
    apply_string_override(env, "TV_BOT__BROKER__DEVICE_ID", |value| {
        partial.broker_mut().device_id = Some(value);
    });
    apply_string_override(env, "TV_BOT__BROKER__PAPER_ACCOUNT_NAME", |value| {
        partial.broker_mut().paper_account_name = Some(value);
    });
    apply_string_override(env, "TV_BOT__BROKER__LIVE_ACCOUNT_NAME", |value| {
        partial.broker_mut().live_account_name = Some(value);
    });

    apply_string_override(env, "TV_BOT__PERSISTENCE__PRIMARY_URL", |value| {
        partial.persistence_mut().primary_url = Some(value);
    });
    apply_string_override(env, "TV_BOT__PERSISTENCE__SQLITE_FALLBACK_PATH", |value| {
        partial
            .persistence_mut()
            .sqlite_fallback
            .get_or_insert_with(PartialSqliteFallbackConfig::default)
            .path = Some(PathBuf::from(value));
    });

    if let Some(value) = env.get("TV_BOT__PERSISTENCE__SQLITE_FALLBACK_ENABLED") {
        partial
            .persistence_mut()
            .sqlite_fallback
            .get_or_insert_with(PartialSqliteFallbackConfig::default)
            .enabled = Some(parse_bool(
            "TV_BOT__PERSISTENCE__SQLITE_FALLBACK_ENABLED",
            &value,
        )?);
    }

    apply_string_override(env, "TV_BOT__CONTROL_API__HTTP_BIND", |value| {
        partial.control_api_mut().http_bind = Some(value);
    });
    apply_string_override(env, "TV_BOT__CONTROL_API__WEBSOCKET_BIND", |value| {
        partial.control_api_mut().websocket_bind = Some(value);
    });
    apply_string_override(env, "TV_BOT__LOGGING__LEVEL", |value| {
        partial.logging_mut().level = Some(value);
    });

    if let Some(value) = env.get("TV_BOT__LOGGING__JSON") {
        partial.logging_mut().json = Some(parse_bool("TV_BOT__LOGGING__JSON", &value)?);
    }

    Ok(())
}

fn apply_string_override(env: &impl Environment, key: &str, update: impl FnOnce(String)) {
    if let Some(value) = env.get(key) {
        update(value);
    }
}

fn parse_runtime_mode(key: &str, value: &str) -> Result<RuntimeMode, ConfigError> {
    match value.trim().to_ascii_lowercase().as_str() {
        "paper" => Ok(RuntimeMode::Paper),
        "live" => Ok(RuntimeMode::Live),
        "observation" => Ok(RuntimeMode::Observation),
        "paused" => Ok(RuntimeMode::Paused),
        _ => Err(ConfigError::InvalidEnvironmentValue {
            key: key.to_owned(),
            value: value.to_owned(),
            message: "expected one of: paper, live, observation, paused".to_owned(),
        }),
    }
}

fn parse_broker_environment(key: &str, value: &str) -> Result<BrokerEnvironment, ConfigError> {
    match value.trim().to_ascii_lowercase().as_str() {
        "demo" => Ok(BrokerEnvironment::Demo),
        "live" => Ok(BrokerEnvironment::Live),
        "custom" => Ok(BrokerEnvironment::Custom),
        _ => Err(ConfigError::InvalidEnvironmentValue {
            key: key.to_owned(),
            value: value.to_owned(),
            message: "expected one of: demo, live, custom".to_owned(),
        }),
    }
}

fn parse_bool(key: &str, value: &str) -> Result<bool, ConfigError> {
    match value.trim().to_ascii_lowercase().as_str() {
        "1" | "true" | "yes" | "on" => Ok(true),
        "0" | "false" | "no" | "off" => Ok(false),
        _ => Err(ConfigError::InvalidEnvironmentValue {
            key: key.to_owned(),
            value: value.to_owned(),
            message: "expected a boolean value".to_owned(),
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn env(entries: &[(&str, &str)]) -> MapEnvironment {
        MapEnvironment::new(
            entries
                .iter()
                .map(|(key, value)| (key.to_string(), value.to_string()))
                .collect(),
        )
    }

    #[test]
    fn loads_defaults_and_required_mode_from_toml() {
        let config = AppConfig::from_toml_str(
            "runtime.example.toml",
            r#"
                [runtime]
                startup_mode = "observation"
            "#,
            &MapEnvironment::default(),
        )
        .expect("config should load");

        assert!(matches!(
            config.runtime.startup_mode,
            RuntimeMode::Observation
        ));
        assert_eq!(config.control_api.http_bind, DEFAULT_HTTP_BIND);
        assert_eq!(config.control_api.websocket_bind, DEFAULT_WEBSOCKET_BIND);
        assert_eq!(config.logging.level, DEFAULT_LOG_LEVEL);
        assert!(!config.persistence.sqlite_fallback.enabled);
        assert_eq!(
            config.persistence.sqlite_fallback.path,
            PathBuf::from(DEFAULT_SQLITE_PATH)
        );
    }

    #[test]
    fn environment_overrides_toml_values() {
        let config = AppConfig::from_toml_str(
            "runtime.example.toml",
            r#"
                [runtime]
                startup_mode = "observation"

                [control_api]
                http_bind = "127.0.0.1:7000"

                [broker]
                environment = "demo"
            "#,
            &env(&[
                ("TV_BOT__RUNTIME__STARTUP_MODE", "paper"),
                ("TV_BOT__CONTROL_API__HTTP_BIND", "127.0.0.1:9000"),
                ("TV_BOT__BROKER__ENVIRONMENT", "live"),
                ("TV_BOT__PERSISTENCE__SQLITE_FALLBACK_ENABLED", "true"),
            ]),
        )
        .expect("config should load");

        assert!(matches!(config.runtime.startup_mode, RuntimeMode::Paper));
        assert_eq!(config.control_api.http_bind, "127.0.0.1:9000");
        assert!(config.persistence.sqlite_fallback.enabled);
        assert_eq!(config.broker.environment, Some(BrokerEnvironment::Live));
    }

    #[test]
    fn missing_explicit_startup_mode_fails() {
        let error = AppConfig::from_toml_str(
            "runtime.example.toml",
            r#"
                [logging]
                level = "debug"
            "#,
            &MapEnvironment::default(),
        )
        .expect_err("config should fail without mode");

        assert!(matches!(
            error,
            ConfigError::MissingRequiredField("runtime.startup_mode")
        ));
    }

    #[test]
    fn invalid_environment_values_fail_fast() {
        let error = AppConfig::from_toml_str(
            "runtime.example.toml",
            r#"
                [runtime]
                startup_mode = "paused"
            "#,
            &env(&[("TV_BOT__LOGGING__JSON", "sometimes")]),
        )
        .expect_err("invalid env should fail");

        match error {
            ConfigError::InvalidEnvironmentValue { key, .. } => {
                assert_eq!(key, "TV_BOT__LOGGING__JSON");
            }
            other => panic!("unexpected error: {other:?}"),
        }
    }

    #[test]
    fn broker_credentials_and_environment_load_from_toml() {
        let config = AppConfig::from_toml_str(
            "runtime.example.toml",
            r#"
                [runtime]
                startup_mode = "paper"

                [broker]
                environment = "demo"
                username = "test-user"
                password = "top-secret"
                cid = "cid-123"
                sec = "sec-456"
            "#,
            &MapEnvironment::default(),
        )
        .expect("config should load");

        assert_eq!(config.broker.environment, Some(BrokerEnvironment::Demo));
        assert_eq!(config.broker.username.as_deref(), Some("test-user"));
        assert_eq!(config.broker.cid.as_deref(), Some("cid-123"));
        assert!(config.broker.password.is_some());
        assert!(config.broker.sec.is_some());
    }
}
