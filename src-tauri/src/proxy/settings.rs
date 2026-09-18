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
use crate::i18n;

/// 建表（供 init_usage 与测试内存库复用）；表已存在时静默跳过。
pub fn init_settings_on(conn: &Connection) -> Result<(), String> {
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS settings (
            key TEXT PRIMARY KEY,
            value TEXT NOT NULL
        );",
    )
    .map_err(|e| i18n::err_args("init_settings_table_failed", &[&e.to_string()]))
}

/// 把配置整体写入 settings 表（事务内逐字段 UPSERT）。
///
/// 写入后调用方负责同步 config.json 镜像（见 lib.rs）。
pub fn save_config(conn: &Connection, cfg: &Config) -> Result<(), String> {
    let v = serde_json::to_value(cfg)
        .map_err(|e| i18n::err_args("serialize_config_failed", &[&e.to_string()]))?;
    let obj = v.as_object().ok_or_else(|| i18n::err("config_not_object"))?;
    let tx = conn
        .unchecked_transaction()
        .map_err(|e| i18n::err_args("settings_tx_failed", &[&e.to_string()]))?;
    for (k, val) in obj {
        conn.execute(
            "INSERT INTO settings (key, value) VALUES (?1, ?2)
             ON CONFLICT(key) DO UPDATE SET value = excluded.value",
            params![k, val.to_string()],
        )
        .map_err(|e| i18n::err_args("settings_write_failed", &[k, &e.to_string()]))?;
    }
    tx.commit()
        .map_err(|e| i18n::err_args("settings_commit_failed", &[&e.to_string()]))
}

/// 从 settings 表读取配置；表为空或某字段缺失时由 `#[serde(default)]` 兜底。
///
/// 兼容旧版：若表内仍存在旧字段 `api_key` 的行，读取后作为首个 Command Code 账户迁入。
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
        Err(e) => log::warn(&format!(
            "{}: {e}",
            i18n::pick("读取设置表失败", "Failed to read the settings table")
        )),
    }
    let legacy = map
        .get("api_key")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());
    let mut cfg = config_from_map(map);
    cfg.migrate_legacy(legacy.as_deref());
    cfg
}

/// 把 settings 表的字段映射反序列化为 `Config`，逐字段容错。
///
/// 直接 `from_value(...).unwrap_or_default()` 会因任一行 value 类型不符而让整份配置
/// 静默回退默认（用户端口/账户/开关全部丢失且可能被写回），故先按单字段反序列化，
/// 只丢弃/记录出错字段，其余字段照常生效。
fn config_from_map(mut map: Map<String, Value>) -> Config {
    // 旧版刷新间隔为毫秒，须在逐字段处理前换算为秒（详见 config 模块注释）
    super::config::migrate_refresh_interval_unit(&mut map);
    // 以默认配置为基底逐字段覆盖：候选值先单独替换进默认副本并整体反序列化，
    // 真正验证它能落到该字段类型。仅比较 JSON 值种类无法发现「同类型但非法」的值
    // （如端口越界、cc_accounts 条目缺 key），那种值会通过校验、却让最终 from_value
    // 整体失败，导致整份配置回退默认。
    let defaults = serde_json::to_value(Config::default())
        .ok()
        .and_then(|v| v.as_object().cloned())
        .unwrap_or_default();
    let mut obj = defaults.clone();
    for (k, v) in map {
        // 未知字段 serde 会忽略，保留以维持旧行为
        if !defaults.contains_key(&k) {
            obj.insert(k, v);
            continue;
        }
        let mut candidate = defaults.clone();
        candidate.insert(k.clone(), v.clone());
        if serde_json::from_value::<Config>(Value::Object(candidate)).is_ok() {
            obj.insert(k, v);
        } else {
            log::warn(&format!(
                "{}: {k}",
                i18n::pick(
                    "设置项取值非法，已忽略并使用默认值",
                    "Invalid setting value; ignored and using default"
                ),
            ));
        }
    }
    serde_json::from_value(Value::Object(obj)).unwrap_or_else(|e| {
        log::warn(&format!(
            "{}: {e}",
            i18n::pick("设置反序列化失败，使用默认配置", "Failed to deserialize settings, using defaults")
        ));
        Config::default()
    })
}

/// 清理旧版 `api_key` 残留行：旧字段已拆分为 `cc_accounts` + `local_api_key`，
/// 该行仅用于一次性迁移（见 `load_config`）。调用方应在迁移完成后执行，
/// 避免每次启动都从旧行重复迁移（例如用户删光账户后旧 key 又「复活」）。
pub fn purge_legacy_api_key(conn: &Connection) -> Result<(), String> {
    conn.execute("DELETE FROM settings WHERE key = 'api_key'", [])
        .map_err(|e| i18n::err_args("purge_legacy_failed", &[&e.to_string()]))?;
    Ok(())
}

/// 一次性完成旧版 `api_key` 行的迁移与清理：迁移结果**落库后**才删除旧行。
///
/// `load_config` 只在内存里把旧 `api_key` 迁入 `cc_accounts`，不会写回设置表；
/// 若在此之前直接 `purge_legacy_api_key`，旧行被删而账户又没落库，升级时用户账户
/// 就永久丢失（只能重新登录/粘贴）。故这里按「读取迁移 → 落库 → 清理」的顺序执行，
/// 且仅在旧行确实存在时才动作，天然幂等。无旧行时直接返回。
pub fn migrate_legacy_api_key(conn: &Connection) -> Result<(), String> {
    let has_legacy: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM settings WHERE key = 'api_key'",
            [],
            |r| r.get(0),
        )
        .map_err(|e| i18n::err_args("count_settings_failed", &[&e.to_string()]))?;
    if has_legacy == 0 {
        return Ok(());
    }
    // load_config 内部会把旧 api_key 迁入 cc_accounts（仅内存），此处显式落库
    let cfg = load_config(conn);
    save_config(conn, &cfg)?;
    purge_legacy_api_key(conn)
}

/// 首次启动迁移：settings 表为空且 config.json 存在时，把文件内容导入 settings 表。
///
/// 仅导入一次；之后 settings 表为唯一数据源，config.json 退化为镜像兜底。
pub fn migrate_from_config(conn: &Connection, config_path: &std::path::Path) -> Result<(), String> {
    let cnt: i64 = conn
        .query_row("SELECT COUNT(*) FROM settings", [], |r| r.get(0))
        .map_err(|e| i18n::err_args("count_settings_failed", &[&e.to_string()]))?;
    if cnt > 0 {
        return Ok(());
    }
    let Ok(text) = std::fs::read_to_string(config_path) else {
        return Ok(()); // 无 config.json，走默认配置
    };
    if serde_json::from_str::<Config>(&text).is_ok() {
        let cfg = Config::load_file(config_path);
        save_config(conn, &cfg)?;
        log::info(i18n::pick(
            "已从 config.json 迁移设置到 SQLite",
            "Migrated settings from config.json to SQLite",
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::proxy::config::Account;
    use rusqlite::Connection;

    /// 建一个已初始化 settings 表的内存库（测试辅助）。
    fn temp_conn() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        init_settings_on(&conn).unwrap();
        conn
    }

    /// 配置整存整取往返：显式写入的字段还原，未写入的走默认值。
    #[test]
    fn config_roundtrip() {
        let conn = temp_conn();
        let mut cfg = Config::default();
        cfg.port = 3999;
        cfg.host = "127.0.0.1".into();
        cfg.zdr = true;
        cfg.cc_accounts = vec![Account {
            key: "user_test_key".into(),
            user_id: "id_test".into(),
            user_name: "Test".into(),
            source: "oauth".into(),
            added_at: 9,
        }];
        cfg.local_api_key = "sk_local_key".into();
        cfg.language = "en".into();
        cfg.stream_idle_timeout_secs = 45;
        cfg.nonstream_idle_timeout_secs = 120;
        save_config(&conn, &cfg).unwrap();

        let got = load_config(&conn);
        assert_eq!(got.port, 3999);
        assert_eq!(got.host, "127.0.0.1");
        assert!(got.zdr);
        // 语言项随配置一同往返（前端首屏防闪语言即依赖此项持久化）
        assert_eq!(got.language, "en");
        // 空闲超时随配置往返（0 是合法值，非 0 值必须原样读写）
        assert_eq!(got.stream_idle_timeout_secs, 45);
        assert_eq!(got.nonstream_idle_timeout_secs, 120);
        assert_eq!(
            got.cc_accounts,
            vec![Account {
                key: "user_test_key".into(),
                user_id: "id_test".into(),
                user_name: "Test".into(),
                source: "oauth".into(),
                added_at: 9,
            }]
        );
        assert_eq!(got.local_api_key, "sk_local_key");
        // 未显式写入的字段走默认值（max_inflight 默认 32；0 表示不限流，不再作默认）
        assert_eq!(got.max_inflight, 32);
        assert!(got.usage_enabled);
    }

    /// 重复保存按 key UPSERT，不产生重复行。
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

    /// 旧版 api_key 行读取时迁入账户，purge 后不再复活。
    #[test]
    fn legacy_api_key_migrated_and_purged() {
        let conn = temp_conn();
        // 模拟旧版设置表：只有 api_key 一行
        conn.execute(
            "INSERT INTO settings (key, value) VALUES ('api_key', '\"user_old_key\"')",
            [],
        )
        .unwrap();

        // 读取时迁移到 cc_accounts（旧 api_key → Account）
        let cfg = load_config(&conn);
        assert_eq!(cfg.cc_accounts.len(), 1);
        assert_eq!(cfg.cc_accounts[0].key, "user_old_key");
        assert!(cfg.cc_accounts[0].user_id.starts_with("legacy-"));

        // 清理旧行后，再次读取不会重复迁移（例如用户之后删光账户）
        purge_legacy_api_key(&conn).unwrap();
        let cfg = load_config(&conn);
        assert!(cfg.cc_accounts.is_empty());
    }

    /// config.json 首启导入一次；settings 非空后不再导入。
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

    /// 单字段类型损坏仅该字段回退默认，其余字段照常生效。
    #[test]
    fn load_config_tolerates_bad_field_types() {        // 单个字段类型损坏不应导致整份配置回退默认
        let conn = temp_conn();
        let mut cfg = Config::default();
        cfg.port = 4567;
        cfg.zdr = true;
        save_config(&conn, &cfg).unwrap();
        // 手工把 zdr 写成字符串（模拟跨版本/手改库）
        conn.execute(
            "UPDATE settings SET value = '\"yes\"' WHERE key = 'zdr'",
            [],
        )
        .unwrap();
        let got = load_config(&conn);
        // 损坏字段回退默认，但 port 等其余字段照常生效
        assert_eq!(got.port, 4567);
        assert!(!got.zdr);
    }

    /// settings 表被删除（异常环境）时读取不 panic，回退默认配置。
    #[test]
    fn load_config_when_table_missing() {
        let conn = temp_conn();
        conn.execute("DROP TABLE settings", []).unwrap();
        let got = load_config(&conn);
        assert_eq!(got.port, Config::default().port);
    }

    /// 同类型但非法的值（端口越界）只让该字段回退默认，其余字段不得被整体重置。
    #[test]
    fn load_config_rejects_same_kind_invalid_value() {
        let conn = temp_conn();
        let mut cfg = Config::default();
        cfg.port = 4567;
        cfg.zdr = true;
        save_config(&conn, &cfg).unwrap();
        // 端口写成越界整数：类型仍是 number，但无法落进 u16
        conn.execute("UPDATE settings SET value = '70000' WHERE key = 'port'", [])
            .unwrap();
        let got = load_config(&conn);
        assert_eq!(got.port, Config::default().port, "非法端口应回退默认");
        assert!(got.zdr, "其余字段不得因单个非法值被重置");
    }

    /// 旧 api_key 行的迁移清理必须原子：账户落库后旧行才删除，账户不丢失。
    #[test]
    fn migrate_legacy_api_key_persists_account_before_purge() {
        let conn = temp_conn();
        conn.execute(
            "INSERT INTO settings (key, value) VALUES ('api_key', '\"user_old_key\"')",
            [],
        )
        .unwrap();
        migrate_legacy_api_key(&conn).unwrap();
        // 旧行已清理，但账户已落库（重开连接读库仍能看到）
        let cfg = load_config(&conn);
        assert_eq!(cfg.cc_accounts.len(), 1);
        assert_eq!(cfg.cc_accounts[0].key, "user_old_key");
        let legacy: i64 = conn
            .query_row("SELECT COUNT(*) FROM settings WHERE key='api_key'", [], |r| r.get(0))
            .unwrap();
        assert_eq!(legacy, 0, "旧 api_key 行应已清理");
        // 幂等：再次调用不新增账户、不报错
        migrate_legacy_api_key(&conn).unwrap();
        assert_eq!(load_config(&conn).cc_accounts.len(), 1);
    }
}
