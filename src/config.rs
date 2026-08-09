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

    /// SSL 模式：disabled / preferred / required / required_ca
    #[serde(default = "default_ssl_mode")]
    pub ssl_mode: String,
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

fn default_port() -> u16 {
    3306
}

fn default_ssl_mode() -> String {
    "disabled".to_string()
}

fn default_max_rows() -> u32 {
    10000
}

impl Config {
    pub fn load(path: &Path) -> Result<Self> {
        let content = std::fs::read_to_string(path)
            .with_context(|| format!("无法读取配置文件: {}", path.display()))?;
        let config: Config = toml::from_str(&content)
            .with_context(|| "配置文件格式错误")?;
        if config.connections.is_empty() {
            anyhow::bail!("配置文件中至少需要一个连接");
        }
        Ok(config)
    }

    pub fn get_connection(&self, name: &str) -> Option<&ConnectionConfig> {
        self.connections.iter().find(|c| c.name == name)
        .or_else(|| {
            if name.is_empty() { self.connections.first() } else { None }
        })
    }
}