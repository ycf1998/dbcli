# dbcli

MySQL CLI 工具，输出 JSON，支持权限分级。

## 安装

编译后将 `dbcli.exe` 和 `dbcli.toml` 放在同一目录。

```bash
cargo build --release
```

## 配置

在 `dbcli.exe` 同目录下创建 `dbcli.toml`：

```toml
[[connections]]
name = "local"
host = "127.0.0.1"
port = 3306
user = "root"
password = "root"
database = "money_pos"
level = "ddl"
note = "本地开发库"
max_rows = 10000      # SELECT 自动 LIMIT，0 表示不限制
connect_timeout = 10  # 连接超时（秒），0 表示默认
query_timeout = 30    # 查询超时（秒），0 表示默认
```

**level 说明**

| level | 允许 |
|-------|------|
| `readonly` | SELECT / SHOW / DESCRIBE / EXPLAIN / WITH |
| `data` | 以上 + INSERT / UPDATE / DELETE / REPLACE |
| `ddl` | 以上 + CREATE / ALTER / DROP / TRUNCATE / RENAME TABLE |

`CREATE DATABASE`、`DROP DATABASE`、`GRANT`、`SHUTDOWN` 等管理操作一律禁止，不受 level 影响。

**SSL 配置（可选）**

```toml
ssl_mode = "required"            # disabled / required / required_ca
ssl_ca = "/path/to/ca.pem"
```

## 用法

```bash
# 查看连接
./dbcli connections

# 列出数据库/表
./dbcli local databases
./dbcli local tables
./dbcli local schema sys_dict

# 执行 SQL
./dbcli local run "SELECT * FROM users WHERE id = 1"

# 从文件执行 SQL
./dbcli local run --file query.sql

# 从 stdin 执行 SQL
echo "SELECT 1" | ./dbcli local run -

# 多语句
./dbcli local run "SELECT 1; SELECT 2"
```

## 输出格式

成功：
```json
{"ok": true, "duration_ms": 12.5, "columns": ["id", "name"], "rows": [["1", "foo"]], "affected_rows": null}
```

失败：
```json
{"ok": false, "error": "权限不足: 需要 Data 级别 ..."}
```