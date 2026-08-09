use anyhow::{Context, Result};
use sqlparser::dialect::MySqlDialect;
use sqlparser::parser::Parser;

/// 验证 SQL 语法是否合法
pub fn validate(sql: &str) -> Result<()> {
    let dialect = MySqlDialect {};
    Parser::parse_sql(&dialect, sql).with_context(|| "SQL 语法错误")?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_valid_select() {
        assert!(validate("SELECT * FROM users").is_ok());
    }

    #[test]
    fn test_invalid_select() {
        assert!(validate("SELEC * FROM users").is_err());
    }

    #[test]
    fn test_valid_insert() {
        assert!(validate("INSERT INTO users (name) VALUES ('a')").is_ok());
    }
}