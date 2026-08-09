use crate::config::Level;

/// 一条 SQL 语句的分析结果
pub enum Classification<'a> {
    /// 允许执行，需要指定权限级别
    Allowed { sql: &'a str, required: Level },
    /// 工具不支持的操作（删库、GRANT、SHUTDOWN 等）
    Blocked { sql: &'a str, reason: &'static str },
}

/// 将 SQL 拆分成多条语句，逐条分析
pub fn parse_statements(sql: &str) -> Vec<Classification<'_>> {
    let mut result = Vec::new();
    let mut chars = sql.char_indices().peekable();
    let mut start = 0;

    while let Some(&(i, ch)) = chars.peek() {
        match ch {
            '\'' | '"' => {
                let quote = ch;
                chars.next();
                while let Some(&(_, c)) = chars.peek() {
                    if c == '\\' { chars.next(); chars.next(); }
                    else if c == quote { chars.next(); break; }
                    else { chars.next(); }
                }
            }
            '`' => {
                chars.next();
                while let Some(&(_, c)) = chars.peek() {
                    if c == '`' { chars.next(); break; }
                    chars.next();
                }
            }
            '-' if sql[i..].starts_with("--") => {
                while let Some(&(_, c)) = chars.peek() {
                    if c == '\n' || c == '\r' { break; }
                    chars.next();
                }
            }
            '/' if sql[i..].starts_with("/*") => {
                chars.next(); chars.next();
                while let Some(&(_, c)) = chars.peek() {
                    if c == '*' && sql.get(chars.peek().map(|&(j, _)| j).unwrap_or(i)..)
                        .map_or(false, |s| s.starts_with("*/"))
                    {
                        chars.next(); chars.next(); break;
                    }
                    chars.next();
                }
            }
            ';' => {
                let stmt = sql[start..i].trim();
                if !stmt.is_empty() {
                    result.push(classify(stmt));
                }
                chars.next();
                start = i + 1;
            }
            _ => { chars.next(); }
        }
    }

    let remaining = sql[start..].trim();
    if !remaining.is_empty() {
        result.push(classify(remaining));
    }

    result
}

fn classify(sql: &str) -> Classification<'_> {
    let upper = sql.to_uppercase();
    let mut parts = upper.split_whitespace();
    let first = match parts.next() {
        Some(w) => w,
        None => return Classification::Blocked { sql, reason: "空语句" },
    };
    let second = parts.next();

    match first {
        "SELECT" | "SHOW" | "DESCRIBE" | "DESC" | "EXPLAIN" | "WITH" => {
            return Classification::Allowed { sql, required: Level::Readonly };
        }
        "INSERT" | "UPDATE" | "DELETE" | "REPLACE" | "CALL" | "DO" | "LOAD" | "HANDLER" => {
            return Classification::Allowed { sql, required: Level::Data };
        }
        "CREATE" | "ALTER" | "DROP" | "TRUNCATE" | "RENAME" => {
            if matches!(second, Some("DATABASE" | "SCHEMA" | "USER" | "ROLE" | "SERVER" | "TABLESPACE" | "LOGFILE")) {
                return Classification::Blocked {
                    sql,
                    reason: "不允许操作数据库级别对象（DATABASE/USER/GRANT 等）",
                };
            }
            return Classification::Allowed { sql, required: Level::Ddl };
        }
        "GRANT" | "REVOKE" | "FLUSH" | "INSTALL" | "UNINSTALL" | "KILL"
        | "SET" | "SHUTDOWN" | "RESET" | "PURGE" | "CHANGE" | "START"
        | "STOP" | "BINLOG" | "CACHE" | "BACKUP" | "RESTORE" => {
            return Classification::Blocked { sql, reason: "管理操作已禁止" };
        }
        _ => {}
    }

    Classification::Blocked { sql, reason: "不被允许的操作类型" }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_select() {
        let r = parse_statements("SELECT * FROM users");
        assert!(matches!(r[0], Classification::Allowed { required: Level::Readonly, .. }));
    }

    #[test]
    fn test_insert() {
        let r = parse_statements("INSERT INTO t VALUES (1)");
        assert!(matches!(r[0], Classification::Allowed { required: Level::Data, .. }));
    }

    #[test]
    fn test_drop_table() {
        let r = parse_statements("DROP TABLE foo");
        assert!(matches!(r[0], Classification::Allowed { required: Level::Ddl, .. }));
    }

    #[test]
    fn test_drop_database_blocked() {
        let r = parse_statements("DROP DATABASE foo");
        assert!(matches!(r[0], Classification::Blocked { .. }));
    }

    #[test]
    fn test_grant_blocked() {
        let r = parse_statements("GRANT ALL ON *.* TO 'u'@'h'");
        assert!(matches!(r[0], Classification::Blocked { .. }));
    }

    #[test]
    fn test_create_table_allowed() {
        let r = parse_statements("CREATE TABLE t (id INT)");
        assert!(matches!(r[0], Classification::Allowed { required: Level::Ddl, .. }));
    }

    #[test]
    fn test_multi_mixed() {
        let r = parse_statements("SELECT 1; DROP DATABASE foo; INSERT INTO t VALUES(1)");
        assert_eq!(r.len(), 3);
        assert!(matches!(r[0], Classification::Allowed { required: Level::Readonly, .. }));
        assert!(matches!(r[1], Classification::Blocked { .. }));
        assert!(matches!(r[2], Classification::Allowed { required: Level::Data, .. }));
    }

    #[test]
    fn test_semicolon_in_string() {
        let r = parse_statements("SELECT 'hello; world'");
        assert_eq!(r.len(), 1);
    }

    #[test]
    fn test_shutdown_blocked() {
        let r = parse_statements("SHUTDOWN");
        assert!(matches!(r[0], Classification::Blocked { .. }));
    }
}