//! 模型信息的持久化层：SQLite `model_pricing` 表（主存）+ 内嵌常量兜底。
//!
//! 与 `settings` 表共用同一个 SQLite 文件（usage.sqlite）。
//!
//! - **首次启动播种**：表为空时把内嵌表（`pricing::builtin_models`）整体写入数据库，
//!   之后运行时读取均以数据库为准；
//! - **数据更新覆盖**：按模型 ID UPSERT，整行 JSON 覆盖旧值（`upsert_models`）；
//! - **最终兜底**：数据库为空或整表解析失败时回退内嵌常量（维护者不定时更新）。
//!
//! 每行存一个模型的完整 JSON（含名称、能力、分档费率、闲忙时与折扣），
//! 与 `pricing::ModelPricing` 一一对应；新增字段只需改类型，无需迁移表结构。

use rusqlite::{params, Connection};

use super::log;
use super::pricing::{self, ModelPricing};
use super::state::now_secs;

/// 建表（供 init_usage 与测试内存库复用）；表已存在时静默跳过。
pub fn init_models_on(conn: &Connection) -> Result<(), String> {
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS model_pricing (
            id TEXT PRIMARY KEY,
            data TEXT NOT NULL,
            source TEXT NOT NULL,
            updated_at INTEGER NOT NULL
        );",
    )
    .map_err(|e| format!("初始化模型表失败: {e}"))
}

/// 首次启动播种：表为空时把内嵌表写入数据库，返回写入条数（非首次返回 0）。
pub fn seed_if_empty(conn: &Connection) -> Result<usize, String> {
    let count: i64 = conn
        .query_row("SELECT COUNT(*) FROM model_pricing", [], |r| r.get(0))
        .map_err(|e| format!("统计模型条数失败: {e}"))?;
    if count > 0 {
        return Ok(0);
    }
    let seeded = upsert_models(conn, pricing::builtin_models(), "builtin")?;
    log::info(&format!("模型信息首次落库：已写入 {seeded} 条内置数据"));
    Ok(seeded)
}

/// 以覆盖语义写入模型信息（数据更新用）：按 ID UPSERT，整行 JSON 覆盖旧值。
///
/// 整批写入包在单个事务内，避免中途失败留下半新半旧的表。
pub fn upsert_models(
    conn: &Connection,
    models: &[ModelPricing],
    source: &str,
) -> Result<usize, String> {
    let tx = conn
        .unchecked_transaction()
        .map_err(|e| format!("开启模型事务失败: {e}"))?;
    let now = now_secs();
    for m in models {
        let data =
            serde_json::to_string(m).map_err(|e| format!("模型 {0} 序列化失败: {e}", m.id))?;
        conn.execute(
            "INSERT INTO model_pricing (id, data, source, updated_at) VALUES (?1, ?2, ?3, ?4)
             ON CONFLICT(id) DO UPDATE SET
                data = excluded.data,
                source = excluded.source,
                updated_at = excluded.updated_at",
            params![m.id, data, source, now as i64],
        )
        .map_err(|e| format!("写入模型 {} 失败: {e}", m.id))?;
    }
    tx.commit().map_err(|e| format!("提交模型事务失败: {e}"))?;
    Ok(models.len())
}

/// 读取全部模型信息（按 ID 排序，保证输出稳定）。
///
/// 表为空或整表解析失败时回退内嵌常量；个别行损坏只跳过该行并告警。
pub fn load_all(conn: &Connection) -> Vec<ModelPricing> {
    let mut out = Vec::new();
    match conn.prepare("SELECT id, data FROM model_pricing ORDER BY id") {
        Ok(mut stmt) => match stmt
            .query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))
        {
            Ok(rows) => {
                for row in rows.flatten() {
                    match serde_json::from_str::<ModelPricing>(&row.1) {
                        Ok(m) => out.push(m),
                        Err(e) => log::warn(&format!("模型 {} 数据解析失败，已跳过: {e}", row.0)),
                    }
                }
            }
            Err(e) => log::warn(&format!("遍历模型表失败: {e}")),
        },
        Err(e) => log::warn(&format!("读取模型表失败: {e}")),
    }
    if out.is_empty() {
        log::warn("模型表为空或不可读，回退内置兜底表");
        return pricing::builtin_models().to_vec();
    }
    out
}

/// 启动时初始化并载入模型表：建表 → 空表播种 → 读取全表，返回当前生效的模型列表。
pub fn init_and_load(conn: &Connection) -> Result<Vec<ModelPricing>, String> {
    init_models_on(conn)?;
    seed_if_empty(conn)?;
    Ok(load_all(conn))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_conn() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        init_models_on(&conn).unwrap();
        conn
    }

    fn sample(id: &str, name: &str) -> ModelPricing {
        ModelPricing {
            id: id.into(),
            name: name.into(),
            category: "premium".into(),
            provider: Some("Test".into()),
            context_window: Some(1000),
            caps: pricing::ModelCaps {
                text: true,
                vision: false,
                reasoning: true,
            },
            deprecated: false,
            deal: None,
            time_of_day: None,
            tiers: vec![pricing::Tier {
                max_context: None,
                rates: pricing::Rates {
                    input: 1.0,
                    output: 2.0,
                    cached: 0.1,
                    cache_write: 0.0,
                },
                list_rates: None,
            }],
        }
    }

    #[test]
    fn seed_only_when_empty() {
        let conn = temp_conn();
        let n = seed_if_empty(&conn).unwrap();
        assert_eq!(n, pricing::builtin_models().len());
        // 再次播种不写入、不覆盖
        assert_eq!(seed_if_empty(&conn).unwrap(), 0);
        assert_eq!(load_all(&conn).len(), pricing::builtin_models().len());
    }

    #[test]
    fn upsert_overwrites_existing_row() {
        let conn = temp_conn();
        upsert_models(&conn, &[sample("m1", "旧名")], "manual").unwrap();
        upsert_models(&conn, &[sample("m1", "新名")], "remote").unwrap();

        let rows = load_all(&conn);
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].name, "新名");
        let source: String = conn
            .query_row("SELECT source FROM model_pricing WHERE id='m1'", [], |r| {
                r.get(0)
            })
            .unwrap();
        assert_eq!(source, "remote");
    }

    #[test]
    fn load_falls_back_to_builtin_when_empty() {
        let conn = temp_conn();
        let rows = load_all(&conn);
        assert_eq!(rows.len(), pricing::builtin_models().len());
        assert!(!rows.is_empty());
    }

    #[test]
    fn init_and_load_seeds_then_reads_db() {
        let conn = temp_conn();
        let rows = init_and_load(&conn).unwrap();
        assert!(rows.len() >= 60, "播种后应拿到完整内置表: {}", rows.len());
        // 落库后新增一条，再载入应包含它（证明读的是数据库而非常量）
        upsert_models(&conn, &[sample("custom-x", "Custom X")], "manual").unwrap();
        let rows = init_and_load(&conn).unwrap();
        assert!(rows.iter().any(|m| m.id == "custom-x"));
    }

    #[test]
    fn bad_row_is_skipped_not_fatal() {
        let conn = temp_conn();
        conn.execute(
            "INSERT INTO model_pricing (id, data, source, updated_at) VALUES ('bad', '{not json', 'manual', 0)",
            [],
        )
        .unwrap();
        upsert_models(&conn, &[sample("good", "Good")], "manual").unwrap();
        let rows = load_all(&conn);
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].id, "good");
    }

    #[test]
    fn persisted_json_round_trips_list_rates() {
        // 促销模型的标牌价（listRates）必须原样往返，落库不能丢字段
        let conn = temp_conn();
        let mut m = sample("promo", "Promo");
        m.tiers[0].list_rates =
            Some(pricing::Rates { input: 5.0, output: 15.0, cached: 1.0, cache_write: 6.26 });
        upsert_models(&conn, &[m], "manual").unwrap();

        let back = &load_all(&conn)[0];
        let list = back.tiers[0].list_rates.expect("listRates 应被保留");
        assert!((list.input - 5.0).abs() < 1e-9);
        assert!((list.output - 15.0).abs() < 1e-9);
        assert!((list.cache_write - 6.26).abs() < 1e-9);
    }

    #[test]
    fn works_on_real_database_file() {
        // 走启动时同一条路径：init_usage 建库（含用量/设置/模型三表）后初始化模型表
        let path = std::env::temp_dir().join(format!("cc-models-{}.sqlite", std::process::id()));
        let _ = std::fs::remove_file(&path);
        let conn = super::super::usage::init_usage(&path).unwrap();
        let models = init_and_load(&conn).unwrap();
        assert!(
            models.len() >= 60,
            "首次启动应播种完整内置表: {}",
            models.len()
        );
        // 用量表与模型表共存于同一库
        let cnt: i64 = conn
            .query_row("SELECT COUNT(*) FROM usage_history", [], |r| r.get(0))
            .unwrap();
        assert_eq!(cnt, 0);
        // 播种是无损的：内置常量里的标牌价（qwen-3.7-max 带 listRates）应原样落库
        let promo = models.iter().find(|m| m.id == "qwen-3.7-max").unwrap();
        assert!(promo.tiers[0].list_rates.is_some(), "播种后不应丢失 listRates");
        let _ = std::fs::remove_file(&path);
    }
}
