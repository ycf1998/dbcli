use crate::config::Level;
use crate::permission::{Classification, parse_statements};

fn classify_first(sql: &str) -> Classification<'_> {
    let r = parse_statements(sql);
    assert_eq!(r.len(), 1, "expected exactly one statement for {sql:?}, got {}", r.len());
    r.into_iter().next().unwrap()
}

fn assert_allowed(sql: &str, expected: Level) {
    match classify_first(sql) {
        Classification::Allowed { required, .. } => assert_eq!(required, expected, "wrong level for {sql:?}"),
        Classification::Blocked { .. } => panic!("expected allowed but blocked: {sql:?}"),
    }
}

fn assert_blocked(sql: &str) {
    match classify_first(sql) {
        Classification::Allowed { required, .. } => {
            panic!("expected blocked but allowed as {required:?}: {sql:?}")
        }
        Classification::Blocked { .. } => {}
    }
}

fn assert_statements(sql: &str, expected: &[Option<Level>]) {
    let r = parse_statements(sql);
    assert_eq!(r.len(), expected.len(), "statement count mismatch for {sql:?}");
    for (i, (stmt, exp)) in r.into_iter().zip(expected.iter()).enumerate() {
        match (stmt, *exp) {
            (Classification::Allowed { required, .. }, Some(expected_level)) => {
                assert_eq!(required, expected_level, "level mismatch at {i} for {sql:?}")
            }
            (Classification::Blocked { .. }, None) => {}
            (Classification::Allowed { required, .. }, None) => {
                panic!("expected blocked at {i} for {sql:?}, but allowed as {required:?}")
            }
            (Classification::Blocked { .. }, Some(expected_level)) => {
                panic!("expected allowed as {expected_level:?} at {i} for {sql:?}, but blocked")
            }
        }
    }
}

// ----------------------------------------------------------------------------
// Readonly 语句
// ----------------------------------------------------------------------------

#[test]
fn select_is_readonly() {
    assert_allowed("SELECT * FROM users", Level::Readonly);
}

#[test]
fn with_cte_is_readonly() {
    assert_allowed("WITH t AS (SELECT 1) SELECT * FROM t", Level::Readonly);
}

#[test]
fn explain_is_readonly() {
    assert_allowed("EXPLAIN SELECT * FROM t", Level::Readonly);
}

#[test]
fn describe_is_readonly() {
    assert_allowed("DESCRIBE t", Level::Readonly);
    assert_allowed("DESC t", Level::Readonly);
}

#[test]
fn show_statements_are_readonly() {
    assert_allowed("SHOW DATABASES", Level::Readonly);
    assert_allowed("SHOW DATABASES LIKE 'qk_money'", Level::Readonly);
    assert_allowed("SHOW TABLES", Level::Readonly);
    assert_allowed("SHOW TABLES LIKE 't%", Level::Readonly);
    assert_allowed("SHOW COLUMNS FROM t", Level::Readonly);
    assert_allowed("SHOW VARIABLES LIKE 'max%'", Level::Readonly);
    assert_allowed("SHOW CREATE TABLE t", Level::Readonly);
    assert_allowed("SHOW INDEX FROM t", Level::Readonly);
    assert_allowed("SHOW PROCESSLIST", Level::Readonly);
    assert_allowed("SHOW STATUS", Level::Readonly);
}

// ----------------------------------------------------------------------------
// Data 语句
// ----------------------------------------------------------------------------

#[test]
fn insert_update_delete_are_data() {
    assert_allowed("INSERT INTO t VALUES (1)", Level::Data);
    assert_allowed("UPDATE t SET a = 1", Level::Data);
    assert_allowed("DELETE FROM t WHERE id = 1", Level::Data);
}

#[test]
fn call_is_data() {
    assert_allowed("CALL my_proc()", Level::Data);
}

#[test]
fn transaction_statements_are_data() {
    assert_allowed("START TRANSACTION", Level::Data);
    assert_allowed("BEGIN", Level::Data);
    assert_allowed("COMMIT", Level::Data);
    assert_allowed("ROLLBACK", Level::Data);
}

// ----------------------------------------------------------------------------
// DDL 语句
// ----------------------------------------------------------------------------

#[test]
fn create_alter_drop_table_are_ddl() {
    assert_allowed("CREATE TABLE t (id INT)", Level::Ddl);
    assert_allowed("ALTER TABLE t ADD COLUMN x INT", Level::Ddl);
    assert_allowed("DROP TABLE t", Level::Ddl);
    assert_allowed("TRUNCATE TABLE t", Level::Ddl);
    assert_allowed("RENAME TABLE t TO t2", Level::Ddl);
}

#[test]
fn create_index_and_view_are_ddl() {
    assert_allowed("CREATE INDEX idx ON t(a)", Level::Ddl);
    assert_allowed("CREATE VIEW v AS SELECT * FROM t", Level::Ddl);
}

// ----------------------------------------------------------------------------
// 任何等级都不允许
// ----------------------------------------------------------------------------

#[test]
fn drop_database_user_schema_role_are_blocked() {
    assert_blocked("DROP DATABASE db");
    assert_blocked("DROP USER 'u'@'h'");
    assert_blocked("DROP SCHEMA s");
    assert_blocked("DROP ROLE r");
}

#[test]
fn create_database_user_schema_role_are_blocked() {
    assert_blocked("CREATE DATABASE db");
    assert_blocked("CREATE USER 'u'@'h' IDENTIFIED BY 'x'");
    assert_blocked("CREATE SCHEMA s");
    assert_blocked("CREATE ROLE r");
}

#[test]
fn admin_statements_are_blocked() {
    assert_blocked("GRANT ALL ON *.* TO 'u'@'h'");
    assert_blocked("REVOKE ALL ON *.* FROM 'u'@'h'");
    assert_blocked("SET NAMES utf8");
    assert_blocked("FLUSH TABLES");
    assert_blocked("KILL 1");
    assert_blocked("SHUTDOWN");
}

#[test]
fn start_slave_is_blocked() {
    assert_blocked("START SLAVE");
}

// ----------------------------------------------------------------------------
// 分句
// ----------------------------------------------------------------------------

#[test]
fn semicolon_in_string_does_not_split() {
    assert_statements("SELECT 'hello; world'", &[Some(Level::Readonly)]);
}

#[test]
fn semicolon_in_single_quote() {
    assert_statements(
        "INSERT INTO t VALUES ('a;b'); SELECT 1",
        &[Some(Level::Data), Some(Level::Readonly)],
    );
}

#[test]
fn semicolon_in_double_quote() {
    assert_statements("SELECT \"a;b\"", &[Some(Level::Readonly)]);
}

#[test]
fn escaped_single_quote() {
    assert_statements(
        "INSERT INTO t VALUES ('it''s a;b'); SELECT 1",
        &[Some(Level::Data), Some(Level::Readonly)],
    );
}

#[test]
fn semicolon_in_line_comment_does_not_split() {
    assert_statements(
        "SELECT 1; -- comment; still comment\nSELECT 2",
        &[Some(Level::Readonly), Some(Level::Readonly)],
    );
}

#[test]
fn semicolon_in_block_comment_does_not_split() {
    assert_statements(
        "/* comment; still */ SELECT 1",
        &[Some(Level::Readonly)],
    );
}

#[test]
fn escaped_backtick() {
    assert_statements("SELECT `a``b`", &[Some(Level::Readonly)]);
}

#[test]
fn minus_minus_in_expression_is_not_comment() {
    assert_statements(
        "SELECT 1--2; SELECT 3",
        &[Some(Level::Readonly), Some(Level::Readonly)],
    );
}

#[test]
fn multiple_statements_mixed() {
    assert_statements(
        "SELECT 1; DROP DATABASE foo; INSERT INTO t VALUES(1)",
        &[Some(Level::Readonly), None, Some(Level::Data)],
    );
}

// ----------------------------------------------------------------------------
// 注释处理
// ----------------------------------------------------------------------------

#[test]
fn line_comment_at_start_is_skipped() {
    assert_allowed("-- comment\nSELECT 1", Level::Readonly);
}

#[test]
fn block_comment_at_start_is_skipped() {
    assert_allowed("/* comment */ SELECT 1", Level::Readonly);
}

#[test]
fn leading_comment_then_multiple_statements() {
    assert_statements(
        "-- comment\nSELECT 1; SELECT 2",
        &[Some(Level::Readonly), Some(Level::Readonly)],
    );
}

// ----------------------------------------------------------------------------
// 边界
// ----------------------------------------------------------------------------

#[test]
fn empty_input_returns_empty() {
    assert_statements("", &[]);
    assert_statements("   ", &[]);
}

#[test]
fn unknown_statement_is_blocked() {
    assert_blocked("FOOBAR");
}
