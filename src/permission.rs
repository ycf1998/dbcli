use crate::config::Level;
use sqlparser::ast::{ObjectType, Statement};
use sqlparser::dialect::{Dialect, MySqlDialect};
use sqlparser::parser::Parser as SqlParser;
use sqlparser::tokenizer::{Token, Tokenizer, TokenWithSpan};

/// 一条 SQL 语句的分析结果
#[derive(Debug)]
pub enum Classification<'a> {
    /// 允许执行，需要指定权限级别
    Allowed { sql: &'a str, required: Level },
    /// 任何权限等级都不允许执行
    Blocked { sql: &'a str },
}

/// 将 SQL 拆分成多条语句，逐条分析
pub fn parse_statements(sql: &str) -> Vec<Classification<'_>> {
    let dialect = MySqlDialect {};

    if let Some(statements) = split_with_sqlparser(sql, &dialect) {
        return statements;
    }

    split_with_state_machine(sql)
        .into_iter()
        .map(classify)
        .collect()
}

/// 使用 sqlparser 按分号精确分句
fn split_with_sqlparser<'a>(sql: &'a str, dialect: &dyn Dialect) -> Option<Vec<Classification<'a>>> {
    let tokens = Tokenizer::new(dialect, sql).tokenize_with_location().ok()?;
    let line_starts = build_line_starts(sql);
    let mut raw_statements: Vec<&str> = Vec::new();
    let mut start = 0;

    for TokenWithSpan { token, span } in tokens {
        if matches!(token, Token::SemiColon) {
            let end = location_to_offset(sql, span.start.line, span.start.column, &line_starts)?;
            let stmt = sql[start..end].trim();
            if !stmt.is_empty() {
                raw_statements.push(stmt);
            }
            start = location_to_offset(sql, span.end.line, span.end.column, &line_starts)?;
        }
    }

    let remaining = sql[start..].trim();
    if !remaining.is_empty() {
        raw_statements.push(remaining);
    }

    let mut result = Vec::new();
    for raw in raw_statements {
        let parsed = SqlParser::parse_sql(dialect, raw).ok()?;
        let stmt = parsed.into_iter().next()?;
        match required_level(&stmt) {
            Some(required) => result.push(Classification::Allowed { sql: raw, required }),
            None => result.push(Classification::Blocked { sql: raw }),
        }
    }

    Some(result)
}

/// 基于 AST 判断语句需要的最低权限等级
/// 返回 None 表示任何等级都不允许
fn required_level(stmt: &Statement) -> Option<Level> {
    match stmt {
        // Readonly
        Statement::Query(_)
        | Statement::ShowVariable { .. }
        | Statement::ShowVariables { .. }
        | Statement::ShowStatus { .. }
        | Statement::ShowDatabases { .. }
        | Statement::ShowSchemas { .. }
        | Statement::ShowCatalogs { .. }
        | Statement::ShowTables { .. }
        | Statement::ShowColumns { .. }
        | Statement::ShowViews { .. }
        | Statement::ShowFunctions { .. }
        | Statement::ShowProcessList { .. }
        | Statement::ShowCollation { .. }
        | Statement::ShowCharset(_)
        | Statement::ShowObjects(_)
        | Statement::ShowCreate { .. }
        | Statement::Explain { .. }
        | Statement::ExplainTable { .. } => Some(Level::Readonly),

        // Data
        Statement::Insert(_)
        | Statement::Update { .. }
        | Statement::Delete(_)
        | Statement::Merge(_)
        | Statement::Call(_)
        | Statement::StartTransaction { .. }
        | Statement::Commit { .. }
        | Statement::Rollback { .. } => Some(Level::Data),

        // DDL
        Statement::CreateTable { .. }
        | Statement::CreateIndex { .. }
        | Statement::CreateView { .. }
        | Statement::AlterTable { .. }
        | Statement::Truncate(_)
        | Statement::RenameTable(_) => Some(Level::Ddl),

        // DROP 按对象类型区分
        Statement::Drop { object_type, .. } => match object_type {
            ObjectType::Database | ObjectType::Schema | ObjectType::Role | ObjectType::User => None,
            _ => Some(Level::Ddl),
        },

        // 其他一律不允许
        _ => None,
    }
}

/// sqlparser 不可用时，退回到字符状态机分句
fn split_with_state_machine(sql: &str) -> Vec<&str> {
    let mut result = Vec::new();
    let mut state = State::Normal;
    let mut start = 0;
    let mut only_comment = true;
    let mut chars = sql.char_indices().peekable();

    while let Some(&(i, ch)) = chars.peek() {
        match state {
            State::Normal => {
                match ch {
                    '\'' | '"' => {
                        state = State::Quoted(ch);
                        only_comment = false;
                    }
                    '`' => {
                        state = State::Backtick;
                        only_comment = false;
                    }
                    '-' if sql[i..].starts_with("--") && is_line_comment_start(sql, i) => {
                        state = State::LineComment;
                    }
                    '/' if sql[i..].starts_with("/*") => state = State::BlockComment,
                    ';' => {
                        let stmt = sql[start..i].trim();
                        if !stmt.is_empty() {
                            result.push(stmt);
                        }
                        start = i + 1;
                        only_comment = true;
                    }
                    c if !c.is_whitespace() => only_comment = false,
                    _ => {}
                }
                chars.next();
            }
            State::Quoted(quote) => {
                if ch == '\\' {
                    chars.next();
                    chars.next();
                } else if ch == quote {
                    chars.next();
                    if !matches!(chars.peek().map(|&(_, c)| c), Some(q) if q == quote) {
                        state = State::Normal;
                    }
                } else {
                    chars.next();
                }
            }
            State::Backtick => {
                chars.next();
                if ch == '`' {
                    if !matches!(chars.peek().map(|&(_, c)| c), Some('`')) {
                        state = State::Normal;
                    }
                }
            }
            State::LineComment => {
                chars.next();
                if ch == '\n' || ch == '\r' {
                    if only_comment {
                        start = i + 1;
                    }
                    state = State::Normal;
                }
            }
            State::BlockComment => {
                if ch == '*' && sql.get(i + 1..).map_or(false, |s| s.starts_with('/')) {
                    if only_comment {
                        start = i + 2;
                    }
                    state = State::Normal;
                    chars.next();
                    chars.next();
                } else {
                    chars.next();
                }
            }
        }
    }

    let remaining = sql[start..].trim();
    if !remaining.is_empty() {
        result.push(remaining);
    }

    result
}

enum State {
    Normal,
    Quoted(char),
    Backtick,
    LineComment,
    BlockComment,
}

/// SQL 行注释 `--` 必须前接空白或位于行首
fn is_line_comment_start(sql: &str, i: usize) -> bool {
    i == 0 || sql[..i].chars().next_back().map_or(false, |c| c.is_whitespace())
}

/// 预计算每行的起始字节偏移，避免每次从头扫描
fn build_line_starts(sql: &str) -> Vec<usize> {
    std::iter::once(0)
        .chain(sql.char_indices().filter(|&(_, c)| c == '\n').map(|(i, _)| i + 1))
        .collect()
}

/// 将 sqlparser 的 (line, column) 转换为字符串 byte offset
fn location_to_offset(sql: &str, line: u64, column: u64, line_starts: &[usize]) -> Option<usize> {
    let line_idx = (line as usize).checked_sub(1)?;
    let start = *line_starts.get(line_idx)?;
    let mut current_col = 1u64;
    for (offset, _) in sql[start..].char_indices() {
        if current_col == column {
            return Some(start + offset);
        }
        current_col += 1;
    }
    if current_col == column {
        Some(sql.len())
    } else {
        None
    }
}

fn classify(sql: &str) -> Classification<'_> {
    let sql = first_non_comment(sql);
    let upper = sql.to_uppercase();
    let mut parts = upper.split_whitespace();
    let first = match parts.next() {
        Some(w) => w,
        None => return Classification::Blocked { sql },
    };
    let second = parts.next();

    let required = match first {
        "SELECT" | "SHOW" | "DESCRIBE" | "DESC" | "EXPLAIN" | "WITH" => Some(Level::Readonly),
        "INSERT" | "UPDATE" | "DELETE" | "REPLACE" | "CALL" | "DO" | "LOAD" | "HANDLER" => Some(Level::Data),
        "CREATE" | "ALTER" | "DROP" | "TRUNCATE" | "RENAME" => {
            if matches!(second, Some("DATABASE" | "SCHEMA" | "USER" | "ROLE" | "SERVER" | "TABLESPACE" | "LOGFILE")) {
                None
            } else {
                Some(Level::Ddl)
            }
        }
        "BEGIN" | "COMMIT" | "ROLLBACK" => Some(Level::Data),
        "START" => {
            if second == Some("TRANSACTION") {
                Some(Level::Data)
            } else {
                None
            }
        }
        "GRANT" | "REVOKE" | "FLUSH" | "INSTALL" | "UNINSTALL" | "KILL"
        | "SET" | "SHUTDOWN" | "RESET" | "PURGE" | "CHANGE" | "STOP"
        | "BINLOG" | "CACHE" | "BACKUP" | "RESTORE" => None,
        _ => None,
    };

    match required {
        Some(required) => Classification::Allowed { sql, required },
        None => Classification::Blocked { sql },
    }
}

/// 跳过开头的空白和单行/多行注释，返回第一个有效词开始的位置
fn first_non_comment(sql: &str) -> &str {
    let mut i = 0;
    let bytes = sql.as_bytes();
    while i < bytes.len() {
        let rest = &sql[i..];
        if rest.starts_with("--") {
            // 行注释：跳到行尾
            if let Some(pos) = rest.find('\n') {
                i += pos + 1;
                continue;
            } else {
                return "";
            }
        }
        if rest.starts_with("/*") {
            // 块注释：跳到 */
            if let Some(pos) = rest.find("*/") {
                i += pos + 2;
                continue;
            } else {
                return "";
            }
        }
        let ch = rest.chars().next().unwrap();
        if ch.is_whitespace() {
            i += ch.len_utf8();
            continue;
        }
        break;
    }
    &sql[i..]
}

#[cfg(test)]
mod tests;
