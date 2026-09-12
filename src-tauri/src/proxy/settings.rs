//! 系统设置的数据层：SQLite `settings` 表（key/value），与流量统计共用同一数据库文件。
//!
//! - 每个配置项一行，`value` 为该字段的 JSON 序列化值；`Config` 整体以对象形式存取，
//!   缺失字段由 `#[serde(default)]` 兜底到默认值。
//! - config.json 保留为「迁移/兜底」源：首次启动且 settings 表为空时自动导入，
//!   之后保存设置时同步写一份镜像，DB 损坏/被删时仍可从 config.json 恢复。

use rusqlite::{params, Connection};
use serde_json::{Map, Value};

use super::config::Config;
use super::log;

/// 建表（供 init_usage 与测试内存库复用）；表已存在时静默跳过。
pub fn init_settings_on(conn: &Connection) -> Result<(), String> {
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS settings (
            key TEXT PRIMARY KEY,
            value TEXT NOT NULL
        );",
    )
    .map_err(|e| format!("初始化设置表失败: {e}"))
}

/// 把配置整体写入 settings 表（事务内逐字段 UPSERT）。
///
/// 写入后调用方负责同步 config.json 镜像（见 lib.rs）。
pub fn save_config(conn: &Connection, cfg: &Config) -> Result<(), String> {
    let v = serde_json::to_value(cfg).map_err(|e| format!("配置序列化失败: {e}"))?;
    let obj = v.as_object().ok_or_else(|| "配置序列化为非对象".to_string())?;
    let tx = conn
        .unchecked_transaction()
        .map_err(|e| format!("开启设置事务失败: {e}"))?;
    for (k, val) in obj {
        conn.execute(
            "INSERT INTO settings (key, value) VALUES (?1, ?2)
             ON CONFLICT(key) DO UPDATE SET value = excluded.value",
            params![k, val.to_string()],
        )
        .map_err(|e| format!("写入设置 {k} 失败: {e}"))?;
    }
    tx.commit().map_err(|e| format!("提交设置事务失败: {e}"))
}

/// 从 settings 表读取配置；表为空或某字段缺失时由 `#[serde(default)]` 兜底。
pub fn load_config(conn: &Connection) -> Config {
    let mut map = Map::new();
    let stmt = conn.prepare("SELECT key, value FROM settings");
    match stmt {
        Ok(mut stmt) => {
            if let Ok(rows) =
                stmt.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))
            {
                for row in rows.flatten() {
                    if let Ok(val) = serde_json::from_str::<Value>(&row.1) {
                        map.insert(row.0, val);
                    }
                }
            }
        }
        Err(e) => log::warn(&format!("读取设置表失败: {e}")),
    }
    serde_json::from_value(Value::Object(map)).unwrap_or_default()
}

/// 首次启动迁移：settings 表为空且 config.json 存在时，把文件内容导入 settings 表。
///
/// 仅导入一次；之后 settings 表为唯一数据源，config.json 退化为镜像兜底。
pub fn migrate_from_config(conn: &Connection, config_path: &std::path::Path) -> Result<(), String> {
    let cnt: i64 = conn
        .query_row("SELECT COUNT(*) FROM settings", [], |r| r.get(0))
        .map_err(|e| format!("统计设置条数失败: {e}"))?;
    if cnt > 0 {
        return Ok(());
    }
    let Ok(text) = std::fs::read_to_string(config_path) else {
        return Ok(()); // 无 config.json，走默认配置
    };
    if serde_json::from_str::<Config>(&text).is_ok() {
        let cfg = Config::load_file(config_path);
        save_config(conn, &cfg)?;
        log::info("已从 config.json 迁移设置到 SQLite");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use rusqlite::Connection;

    fn temp_conn() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        init_settings_on(&conn).unwrap();
        conn
    }

    #[test]
    fn config_roundtrip() {
        let conn = temp_conn();
        let mut cfg = Config::default();
        cfg.port = 3999;
        cfg.host = "127.0.0.1".into();
        cfg.zdr = true;
        cfg.api_key = "user_test_key".into();
        save_config(&conn, &cfg).unwrap();

        let got = load_config(&conn);
        assert_eq!(got.port, 3999);
        assert_eq!(got.host, "127.0.0.1");
        assert!(got.zdr);
        assert_eq!(got.api_key, "user_test_key");
        // 未显式写入的字段走默认值
        assert_eq!(got.max_inflight, 0);
        assert!(got.usage_enabled);
    }

    #[test]
    fn save_upserts_not_duplicates() {
        let conn = temp_conn();
        let mut cfg = Config::default();
        cfg.port = 1000;
        save_config(&conn, &cfg).unwrap();
        let cnt: i64 = conn
            .query_row("SELECT COUNT(*) FROM settings WHERE key='port'", [], |r| r.get(0))
            .unwrap();
        assert_eq!(cnt, 1);
        cfg.port = 2000;
        save_config(&conn, &cfg).unwrap();
        let cnt: i64 = conn
            .query_row("SELECT COUNT(*) FROM settings WHERE key='port'", [], |r| r.get(0))
            .unwrap();
        assert_eq!(cnt, 1);
        assert_eq!(load_config(&conn).port, 2000);
    }

    #[test]
    fn migrate_only_when_empty() {
        let conn = temp_conn();
        let dir = std::env::temp_dir();
        let path = dir.join(format!("cc-settings-migrate-{}.json", std::process::id()));
        let _ = std::fs::remove_file(&path);

        // 无 config.json：不报错，走默认
        migrate_from_config(&conn, &path).unwrap();

        // 有 config.json：导入
        std::fs::write(&path, r#"{"port":4321,"zdr":true}"#).unwrap();
        migrate_from_config(&conn, &path).unwrap();
        let cfg = load_config(&conn);
        assert_eq!(cfg.port, 4321);
        assert!(cfg.zdr);

        // settings 非空：不再导入（保留既有值）
        let mut c2 = Config::default();
        c2.port = 9999;
        save_config(&conn, &c2).unwrap();
        migrate_from_config(&conn, &path).unwrap();
        assert_eq!(load_config(&conn).port, 9999);

        let _ = std::fs::remove_file(&path);
    }
}
