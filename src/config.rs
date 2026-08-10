use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::path::Path;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Deserialize, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Level {
    Readonly = 0,
    Data = 1,
    Ddl = 2,
}

/// TLS 模式
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SslMode {
    /// 不启用 TLS
    #[default]
    Disabled,
    /// 启用 TLS；若配置了 `ssl_ca` 则校验证书
    Required,
    /// 启用 TLS，且必须配置 `ssl_ca`
    RequiredCa,
}

#[derive(Debug, Deserialize)]
pub struct Config {
    pub connections: Vec<ConnectionConfig>,
}

#[derive(Debug, Deserialize, Clone, Serialize)]
pub struct ConnectionConfig {
    pub name: String,
    pub host: String,
    #[serde(default = "default_port")]
    pub port: u16,
    pub user: String,
    #[serde(skip_serializing)]
    pub password: String,
    pub database: Option<String>,
    pub level: Level,
    pub note: Option<String>,

    /// TLS 模式：`disabled` / `required` / `required_ca`
    #[serde(default)]
    pub ssl_mode: SslMode,
    /// CA 证书路径
    pub ssl_ca: Option<String>,

    /// SELECT 自动 LIMIT，0 表示不限制
    #[serde(default = "default_max_rows")]
    pub max_rows: u32,
    /// 连接超时（秒），0 表示默认
    #[serde(default)]
    pub connect_timeout: u64,
    /// 查询超时（秒），0 表示默认
    #[serde(default)]
    pub query_timeout: u64,
}

const fn default_port() -> u16 {
    3306
}

const fn default_max_rows() -> u32 {
    10000
}

impl Config {
    pub fn load(path: &Path) -> Result<Self> {
        let content = std::fs::read_to_string(path)
            .with_context(|| format!("无法读取配置文件: {}", path.display()))?;
        Self::parse(&content)
    }

    fn parse(content: &str) -> Result<Self> {
        let config: Self = toml::from_str(content).with_context(|| "配置文件格式错误")?;
        if config.connections.is_empty() {
            anyhow::bail!("配置文件中至少需要一个连接");
        }
        Ok(config)
    }

    /// 按名称查找连接；名称为空时返回第一个连接。
    pub fn get_connection(&self, name: &str) -> Option<&ConnectionConfig> {
        if name.is_empty() {
            return self.connections.first();
        }
        self.connections.iter().find(|c| c.name == name)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn level_ordering_matches_privilege_escalation() {
        assert!(Level::Readonly < Level::Data);
        assert!(Level::Data < Level::Ddl);
    }

    #[test]
    fn ssl_mode_defaults_to_disabled() {
        assert_eq!(SslMode::default(), SslMode::Disabled);
    }

    #[test]
    fn empty_name_returns_first_connection() {
        let config = Config::parse(
            r#"
            [[connections]]
            name = "a"
            host = "127.0.0.1"
            user = "root"
            password = "p"
            level = "readonly"

            [[connections]]
            name = "b"
            host = "127.0.0.2"
            user = "root"
            password = "p"
            level = "data"
        "#,
        )
        .unwrap();

        assert_eq!(config.get_connection("").unwrap().name, "a");
        assert_eq!(config.get_connection("b").unwrap().name, "b");
        assert!(config.get_connection("missing").is_none());
    }

    #[test]
    fn unknown_ssl_mode_is_rejected() {
        let err = Config::parse(
            r#"
            [[connections]]
            name = "a"
            host = "127.0.0.1"
            user = "root"
            password = "p"
            level = "readonly"
            ssl_mode = "require"
        "#,
        )
        .unwrap_err();

        assert!(err.to_string().contains("配置文件格式错误"), "{err}");
    }

    #[test]
    fn empty_connection_list_is_rejected() {
        assert!(Config::parse("connections = []").is_err());
    }
}
