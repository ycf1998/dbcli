use anyhow::{bail, Context, Result};
use clap::{CommandFactory, Parser, Subcommand};
use mysql::prelude::*;
use mysql::{OptsBuilder, SslOpts};
use std::time::{Duration, Instant};

mod config;
mod permission;

use config::{ConnectionConfig, Level, SslMode};
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

fn load_sql(file: Option<&str>, sql: Option<&str>) -> Result<String> {
    match (file, sql) {
        (Some(_), Some(_)) => bail!("--file 和直接传入 SQL 不能同时使用"),
        (Some("-"), None) | (None, Some("-")) => read_stdin(),
        (Some(path), None) => {
            std::fs::read_to_string(path).with_context(|| format!("无法读取文件: {path}"))
        }
        (None, Some(sql)) => Ok(sql.to_owned()),
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
        println!(
            "{}",
            serde_json::json!({"version": env!("CARGO_PKG_VERSION"), "ok": true})
        );
        return Ok(());
    }

    let Some(cmd) = cli.command else {
        Cli::command().print_help()?;
        println!();
        return Ok(());
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
        .with_context(|| format!("配置中未找到连接 '{conn_name}'"))?;

    // 把快捷命令转成 SQL
    let sql = match &cmd {
        Command::Run { file, sql } => load_sql(file.as_deref(), sql.as_deref())?,
        Command::Tables => "SHOW TABLES".to_string(),
        Command::Schema { table } => format!("DESCRIBE `{table}`"),
        Command::Databases => "SHOW DATABASES".to_string(),
        Command::Connections => unreachable!(),
    };

    // 权限分类并检查
    let mut allowed: Vec<(&str, Level, bool)> = Vec::new();
    for stmt in permission::parse_statements(&sql) {
        match stmt {
            Classification::Blocked { sql } => bail!("不被允许的操作类型: [{sql}]"),
            Classification::Allowed { sql, required, auto_limit } => {
                if required > conn_cfg.level {
                    bail!(
                        "权限不足: 需要 {required:?} 级别，当前连接 '{}' 为 {:?}: [{sql}]",
                        conn_cfg.name,
                        conn_cfg.level
                    );
                }
                allowed.push((sql, required, auto_limit));
            }
        }
    }

    if allowed.is_empty() {
        bail!("没有可执行的 SQL 语句");
    }

    // 连接 MySQL（databases 命令不需要选库）
    let use_database = !matches!(cmd, Command::Databases);
    let mut conn = mysql::Conn::new(build_opts(conn_cfg, use_database)?)
        .with_context(|| "连接 MySQL 失败")?;

    // 失败时回滚，避免事务悬停
    if let Err(e) = execute(&mut conn, &allowed, conn_cfg.max_rows) {
        let _ = conn.exec_drop("ROLLBACK", ());
        return Err(e);
    }

    Ok(())
}

/// 按顺序执行所有语句，每条语句输出一行 JSON。
fn execute(conn: &mut mysql::Conn, statements: &[(&str, Level, bool)], max_rows: u32) -> Result<()> {
    for (i, (sql, required, auto_limit)) in statements.iter().enumerate() {
        let start = Instant::now();
        // Readonly 级别都是返回结果集的查询（SELECT / SHOW / DESCRIBE / EXPLAIN）
        let (columns, rows, affected_rows) = if *required == Level::Readonly {
            let guarded = maybe_limit(sql, max_rows, *auto_limit);
            let result: Vec<mysql::Row> = conn
                .query(&guarded)
                .with_context(|| format!("第 {} 条语句执行失败", i + 1))?;

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
            conn.exec_drop(*sql, ())
                .with_context(|| format!("第 {} 条语句执行失败", i + 1))?;
            (None, None, Some(conn.affected_rows()))
        };

        let duration_ms = start.elapsed().as_secs_f64() * 1000.0;
        println!(
            "{}",
            serde_json::json!({
                "ok": true,
                "duration_ms": duration_ms,
                "columns": columns,
                "rows": rows,
                "affected_rows": affected_rows,
            })
        );
    }
    Ok(())
}

/// 构造连接参数；`use_database` 为假时不指定库（如 `SHOW DATABASES`）。
fn build_opts(cfg: &ConnectionConfig, use_database: bool) -> Result<OptsBuilder> {
    let mut opts = OptsBuilder::new()
        .ip_or_hostname(Some(&cfg.host))
        .tcp_port(cfg.port)
        .user(Some(&cfg.user))
        .pass(Some(&cfg.password))
        .tcp_connect_timeout(
            (cfg.connect_timeout > 0).then(|| Duration::from_secs(cfg.connect_timeout)),
        )
        .read_timeout((cfg.query_timeout > 0).then(|| Duration::from_secs(cfg.query_timeout)));
    if use_database {
        opts = opts.db_name(cfg.database.as_deref());
    }
    apply_ssl(opts, cfg)
}

fn apply_ssl(opts: OptsBuilder, cfg: &ConnectionConfig) -> Result<OptsBuilder> {
    let ssl = match cfg.ssl_mode {
        SslMode::Disabled => return Ok(opts),
        SslMode::Required => match &cfg.ssl_ca {
            Some(ca) => SslOpts::default().with_root_cert_path(Some(std::path::PathBuf::from(ca))),
            None => SslOpts::default(),
        },
        SslMode::RequiredCa => {
            let Some(ca) = &cfg.ssl_ca else {
                bail!(
                    "连接 '{}' 的 ssl_mode = required_ca，但未配置 ssl_ca",
                    cfg.name
                );
            };
            SslOpts::default().with_root_cert_path(Some(std::path::PathBuf::from(ca)))
        }
    };
    Ok(opts.ssl_opts(Some(ssl)))
}

/// 给只读查询追加 `LIMIT`，避免大表全量返回。
///
/// `auto_limit` 由权限分类阶段依据 AST 判定（见 `permission::can_auto_limit`），
/// 只有确定可安全追加时才加，避免破坏 `FOR UPDATE`、已有 LIMIT 等语句。
fn maybe_limit(sql: &str, max_rows: u32, auto_limit: bool) -> String {
    if max_rows == 0 || !auto_limit {
        return sql.to_string();
    }
    // 用换行分隔：若语句以行注释结尾，追加的 LIMIT 不会被注释吞掉
    format!("{sql}\nLIMIT {max_rows}")
}

#[cfg(test)]
mod tests {
    use super::maybe_limit;

    #[test]
    fn appends_limit_when_enabled() {
        assert_eq!(
            maybe_limit("SELECT * FROM t", 100, true),
            "SELECT * FROM t\nLIMIT 100"
        );
    }

    #[test]
    fn skips_limit_when_not_safe_to_append() {
        assert_eq!(maybe_limit("SELECT * FROM t", 100, false), "SELECT * FROM t");
    }

    #[test]
    fn skips_limit_when_max_rows_is_zero() {
        assert_eq!(maybe_limit("SELECT * FROM t", 0, true), "SELECT * FROM t");
    }

    #[test]
    fn appended_limit_is_not_swallowed_by_trailing_line_comment() {
        let out = maybe_limit("SELECT 1 -- note", 10, true);
        assert_eq!(out.lines().last(), Some("LIMIT 10"), "{out}");
    }
}
