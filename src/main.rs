use anyhow::{bail, Context, Result};
use clap::{CommandFactory, Parser, Subcommand};
use mysql::prelude::*;
use mysql::{OptsBuilder, SslOpts};
use std::time::Instant;

mod config;
mod permission;

use config::ConnectionConfig;
use config::Level;
use permission::Classification;

#[derive(Parser)]
#[command(
    name = "dbcli",
    about = "MySQL CLI 工具",
    after_help = "Examples:\n  # 使用默认连接执行 SQL\n  dbcli run \"SELECT 1\"\n\n  # 指定连接执行 SQL\n  dbcli myconn run \"SELECT 1\"\n\n  # 执行文件中的 SQL\n  dbcli run -f script.sql\n\n  # 通过 stdin 执行 SQL\n  cat script.sql | dbcli run -f -\n\n  # 列出连接 / 数据库 / 表\n  dbcli connections\n  dbcli databases\n  dbcli tables\n\n  # 查看表结构\n  dbcli schema my_table"
)]
struct Cli {
    /// 显示版本号
    #[arg(short = 'v', long = "version")]
    version: bool,

    /// 配置中的连接名，省略则用第一个连接
    connection: Option<String>,

    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Subcommand)]
enum Command {
    /// 执行 SQL
    Run {
        /// 从文件读取 SQL
        #[arg(short = 'f', long = "file")]
        file: Option<String>,
        /// 直接传入 SQL
        sql: Option<String>,
    },
    /// 列出配置文件中的所有可用连接
    Connections,
    /// 列出所有数据库
    Databases,
    /// 列出所有表
    Tables,
    /// 查看表结构
    Schema { table: String },
}

fn load_sql(file: &Option<String>, sql: &Option<String>) -> Result<String> {
    match (file, sql) {
        (Some(_), Some(_)) => bail!("--file 和直接传入 SQL 不能同时使用"),
        (Some(path), None) if path == "-" => read_stdin(),
        (Some(path), None) => std::fs::read_to_string(path)
            .with_context(|| format!("无法读取文件: {path}")),
        (None, Some(sql)) if sql == "-" => read_stdin(),
        (None, Some(sql)) => Ok(sql.clone()),
        (None, None) => bail!("请提供 SQL 或 --file"),
    }
}

fn read_stdin() -> Result<String> {
    use std::io::Read;
    let mut buf = String::new();
    std::io::stdin()
        .read_to_string(&mut buf)
        .with_context(|| "无法从 stdin 读取 SQL")?;
    Ok(buf)
}

fn main() {
    if let Err(e) = run() {
        let err = format!("{e:#}");
        println!("{}", serde_json::json!({"error": err, "ok": false}));
        std::process::exit(1);
    }
}

fn run() -> Result<()> {
    let cli = Cli::parse();

    if cli.version {
        println!("{}", serde_json::json!({"version": "1.0.0", "ok": true}));
        return Ok(());
    }

    let cmd = match cli.command {
        Some(c) => c,
        None => {
            let mut app = Cli::command();
            app.print_help()?;
            println!();
            return Ok(());
        }
    };

    // 配置文件和 exe 同目录
    let config_path = std::env::current_exe()
        .context("无法获取 exe 路径")?
        .parent()
        .context("无法获取 exe 目录")?
        .join("dbcli.toml");

    let config = config::Config::load(&config_path)?;

    // connections 命令不需要数据库连接
    if matches!(cmd, Command::Connections) {
        println!("{}", serde_json::json!({"ok": true, "connections": config.connections}));
        return Ok(());
    }

    let conn_name = cli.connection.as_deref().unwrap_or("");
    let conn_cfg = config
        .get_connection(conn_name)
        .with_context(|| format!("配置中未找到连接 '{}'", conn_name))?;

    // 把快捷命令转成 SQL
    let sql = match &cmd {
        Command::Run { file, sql } => load_sql(file, sql)?,
        Command::Tables => "SHOW TABLES".to_string(),
        Command::Schema { table } => format!("DESCRIBE `{table}`"),
        Command::Databases => "SHOW DATABASES".to_string(),
        Command::Connections => unreachable!(),
    };

    // 权限分类并检查
    let mut allowed_statements: Vec<(&str, Level)> = Vec::new();
    for stmt in permission::parse_statements(&sql) {
        match stmt {
            Classification::Blocked { sql } => {
                bail!("不被允许的操作类型: [{}]", sql);
            }
            Classification::Allowed { sql, required } => {
                if required > conn_cfg.level {
                    bail!(
                        "权限不足: 需要 {required:?} 级别，当前连接 '{}' 为 {:?}: [{}]",
                        conn_cfg.name,
                        conn_cfg.level,
                        sql
                    );
                }
                allowed_statements.push((sql, required));
            }
        }
    }

    if allowed_statements.is_empty() {
        bail!("没有可执行的 SQL 语句");
    }

    // 连接 MySQL（databases 命令不需要选库）
    let mut opts_builder = OptsBuilder::new()
        .ip_or_hostname(Some(&conn_cfg.host))
        .tcp_port(conn_cfg.port)
        .user(Some(&conn_cfg.user))
        .pass(Some(&conn_cfg.password))
        .tcp_connect_timeout((conn_cfg.connect_timeout > 0).then(|| std::time::Duration::from_secs(conn_cfg.connect_timeout)))
        .read_timeout((conn_cfg.query_timeout > 0).then(|| std::time::Duration::from_secs(conn_cfg.query_timeout)));
    if !matches!(cmd, Command::Databases) {
        opts_builder = opts_builder.db_name(conn_cfg.database.as_deref());
    }
    apply_ssl(&mut opts_builder, conn_cfg);

    let mut conn = mysql::Conn::new(opts_builder).with_context(|| "连接 MySQL 失败")?;

    // 执行所有语句；失败时回滚，避免事务悬停
    let exec = |conn: &mut mysql::Conn| -> Result<()> {
        for (i, (sql, required)) in allowed_statements.iter().enumerate() {
            let start = Instant::now();
            let is_query = *required == Level::Readonly;

            let (columns, rows, affected_rows) = if is_query {
                let guarded = maybe_limit(sql, conn_cfg.max_rows);
                let result: Vec<mysql::Row> = conn.query(&guarded).with_context(|| {
                    format!("第 {} 条语句执行失败", i + 1)
                })?;

                let columns: Vec<String> = result
                    .first()
                    .map(|row| {
                        row.columns_ref()
                            .iter()
                            .map(|c| c.name_str().to_string())
                            .collect()
                    })
                    .unwrap_or_default();

                let rows: Vec<Vec<serde_json::Value>> = result
                    .into_iter()
                    .map(|row| {
                        (0..row.len())
                            .map(|i| match row.get::<Option<String>, usize>(i) {
                                Some(Some(v)) => serde_json::Value::String(v),
                                _ => serde_json::Value::Null,
                            })
                            .collect()
                    })
                    .collect();

                (Some(columns), Some(rows), None)
            } else {
                conn.exec_drop(*sql, ()).with_context(|| {
                    format!("第 {} 条语句执行失败", i + 1)
                })?;
                (None, None, Some(conn.affected_rows()))
            };

            let duration_ms = start.elapsed().as_secs_f64() * 1000.0;
            let output = serde_json::json!({
                "ok": true,
                "duration_ms": duration_ms,
                "columns": columns,
                "rows": rows,
                "affected_rows": affected_rows,
            });
            println!("{output}");
        }
        Ok(())
    };

    if let Err(e) = exec(&mut conn) {
        let _ = conn.exec_drop("ROLLBACK", ());
        return Err(e);
    }

    Ok(())
}

fn apply_ssl(opts_builder: &mut OptsBuilder, cfg: &ConnectionConfig) {
    if cfg.ssl_mode == "disabled" || cfg.ssl_mode.is_empty() {
        return;
    }

    if cfg.ssl_mode == "required" || cfg.ssl_mode == "required_ca" {
        let mut ssl = SslOpts::default();
        if let Some(ca) = &cfg.ssl_ca {
            let path: std::path::PathBuf = ca.into();
            ssl = ssl.with_root_cert_path(Some(path));
        }
        *opts_builder = opts_builder.clone().ssl_opts(Some(ssl));
    }
}

fn maybe_limit(sql: &str, max_rows: u32) -> String {
    if max_rows == 0 {
        return sql.to_string();
    }
    // 只给简单的 SELECT / WITH 加 LIMIT，避免影响子查询等复杂 SQL
    let upper = sql.to_uppercase();
    let is_readonly_query = upper.starts_with("SELECT") || upper.starts_with("WITH");
    if is_readonly_query && !upper.split_whitespace().any(|w| w == "LIMIT") {
        return format!("{sql} LIMIT {max_rows}");
    }
    sql.to_string()
}
