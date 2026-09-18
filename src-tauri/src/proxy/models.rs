//! 模型信息的持久化层：列表与价格分表存储，共用同一个 SQLite 文件（usage.sqlite）。
//!
//! - **模型列表（`models` 表）**：模型 ID / 名称 / 厂商 / 上下文长度 / 能力。
//!   启动时把内置列表文件（`models.json`，维护者整理、随版本发布）写入数据库；
//!   运行中由 Provider 端点（`/provider/v1/models`）拉取后整表替换刷新；
//! - **模型价格（`model_pricing` 表）**：促销 / 分档费率 / 闲忙时，首次启动由
//!   内嵌价格文件（`pricing.json`）播种，之后数据更新按模型 ID UPSERT（`upsert_pricing`）；
//! - **最终兜底**：表为空或整表解析失败时回退内置数据（`models.json` / `pricing.json`，
//!   由 `tools/fetch-pricing.mjs` 从官方文档爬取生成，维护者不定期更新）。
//!
//! 两张表按模型 ID 关联：列表 ID 为上游原样（可能带厂商前缀，如
//! `tencent/hy3-paid`），价格表 ID 为归一化键，匹配口径见 `pricing::find_pricing`。
//!
//! 历史兼容：旧版把列表与价格合存在 `model_pricing.data` 一个 JSON 里，启动时
//! 自动拆分迁移到两张新表后删除旧表（`migrate_legacy_if_needed`）。

use crate::i18n;
use rusqlite::{params, Connection};
use serde::Deserialize;
use std::sync::OnceLock;

use super::log;
use super::pricing::{self, FullModelRecord, ModelCaps, ModelPricing};
use super::state::{now_secs, AppState, ModelInfo};

/// 内置模型列表文件（维护者整理，随版本发布）：启动时写入 `models` 表，
/// 也是列表读取失败时的兜底数据。
const MODELS_LIST_JSON: &str = include_str!("models.json");

// ── 建表与迁移 ──────────────────────────────────────────────

/// 建表（供 init_usage 与测试内存库复用）；含旧版合并表到双表的自动迁移。
pub fn init_models_on(conn: &Connection) -> Result<(), String> {
    migrate_legacy_if_needed(conn)?;
    create_tables(conn)
}

/// 建两张模型表；表已存在时静默跳过。
fn create_tables(conn: &Connection) -> Result<(), String> {
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS models (
            id TEXT PRIMARY KEY,
            name TEXT NOT NULL,
            provider TEXT,
            context_length INTEGER,
            caps TEXT NOT NULL,
            source TEXT NOT NULL,
            updated_at INTEGER NOT NULL
        );
        CREATE TABLE IF NOT EXISTS model_pricing (
            id TEXT PRIMARY KEY,
            deal TEXT,
            tiers TEXT NOT NULL,
            time_of_day TEXT,
            source TEXT NOT NULL,
            updated_at INTEGER NOT NULL
        );",
    )
    .map_err(|e| {
        let e = e.to_string();
        i18n::err_args("init_models_table_failed", &[&e])
    })
}

/// 检测旧版合并表：`model_pricing` 存在且带 `data` 列。
fn legacy_table_exists(conn: &Connection) -> Result<bool, String> {
    let exists: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name='model_pricing'",
            [],
            |r| r.get(0),
        )
        .map_err(|e| {
            let e = e.to_string();
            i18n::err_args("count_models_failed", &[&e])
        })?;
    if exists == 0 {
        return Ok(false);
    }
    let mut stmt = conn
        .prepare("PRAGMA table_info(model_pricing)")
        .map_err(|e| {
            let e = e.to_string();
            i18n::err_args("count_models_failed", &[&e])
        })?;
    let cols = stmt
        .query_map([], |r| r.get::<_, String>(1))
        .map_err(|e| {
            let e = e.to_string();
            i18n::err_args("count_models_failed", &[&e])
        })?
        .flatten()
        .collect::<Vec<String>>();
    Ok(cols.iter().any(|c| c == "data"))
}

/// 旧版合并表迁移：改名备份 → 建新表 → 逐行拆分写入 → 删除备份。
///
/// 整个过程包在单个事务里，中途失败（含坏行解析失败）整体回滚、旧表保留，下次启动重试。
fn migrate_legacy_if_needed(conn: &Connection) -> Result<(), String> {
    if !legacy_table_exists(conn)? {
        return Ok(());
    }
    let tx = conn
        .unchecked_transaction()
        .map_err(|e| {
            let e = e.to_string();
            i18n::err_args("models_tx_failed", &[&e])
        })?;
    tx.execute_batch("ALTER TABLE model_pricing RENAME TO model_pricing_legacy;")
        .map_err(|e| {
            let e = e.to_string();
            i18n::err_args("models_tx_failed", &[&e])
        })?;
    create_tables(&tx)?;

    let mut stmt = tx
        .prepare("SELECT data, source FROM model_pricing_legacy")
        .map_err(|e| {
            let e = e.to_string();
            i18n::err_args("count_models_failed", &[&e])
        })?;
    let rows = stmt
        .query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))
        .map_err(|e| {
            let e = e.to_string();
            i18n::err_args("count_models_failed", &[&e])
        })?
        .flatten()
        .collect::<Vec<(String, String)>>();
    drop(stmt);

    let now = now_secs();
    let mut migrated = 0usize;
    for (data, source) in rows {
        // 任一行解析失败即整体失败并回滚：旧表迁移的语义是「全有或全无」，
        // 若跳过坏行后仍删除旧表，这些模型/价格将永久丢失且无法重试
        let rec = serde_json::from_str::<FullModelRecord>(&data).map_err(|e| {
            format!(
                "{}: {e}",
                i18n::pick(
                    "旧版模型表存在无法解析的行，迁移已中止（旧表保留，可稍后重试）",
                    "A legacy model row could not be parsed; migration aborted (legacy table kept for retry)"
                )
            )
        })?;
        let entry = entry_from_record(&rec);
        tx.execute(
            "INSERT OR REPLACE INTO models
                (id, name, provider, context_length, caps, source, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            params![
                entry.id,
                entry.name,
                entry.provider,
                entry.context_length.map(|c| c as i64),
                serde_json::to_string(&entry.caps).unwrap_or_default(),
                source,
                now as i64,
            ],
        )
        .map_err(|e| {
            let e = e.to_string();
            i18n::err_args("model_write_failed", &[&rec.id, &e])
        })?;
        let price = pricing_from_record(&rec);
        tx.execute(
            "INSERT OR REPLACE INTO model_pricing
                (id, deal, tiers, time_of_day, source, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![
                price.id,
                price.deal.as_ref().and_then(|d| serde_json::to_string(d).ok()),
                serde_json::to_string(&price.tiers).unwrap_or_default(),
                price
                    .time_of_day
                    .as_ref()
                    .and_then(|t| serde_json::to_string(t).ok()),
                source,
                now as i64,
            ],
        )
        .map_err(|e| {
            let e = e.to_string();
            i18n::err_args("model_write_failed", &[&rec.id, &e])
        })?;
        migrated += 1;
    }
    tx.execute_batch("DROP TABLE model_pricing_legacy;")
        .map_err(|e| {
            let e = e.to_string();
            i18n::err_args("models_tx_failed", &[&e])
        })?;
    tx.commit().map_err(|e| {
        let e = e.to_string();
        i18n::err_args("models_commit_failed", &[&e])
    })?;
    if migrated > 0 {
        log::info(&format!(
            "{} {migrated}",
            i18n::pick(
                "旧版模型合并表已迁移拆分，迁移条数：",
                "Legacy combined model table migrated, rows:"
            )
        ));
    }
    Ok(())
}

// ── 记录转换与厂商推导 ──────────────────────────────────────

/// 内嵌记录 → 列表条目（厂商缺省时按 ID 前缀推导）。
fn entry_from_record(rec: &FullModelRecord) -> ModelInfo {
    ModelInfo {
        id: rec.id.clone(),
        name: if rec.name.is_empty() {
            rec.id.clone()
        } else {
            rec.name.clone()
        },
        provider: rec
            .provider
            .clone()
            .or_else(|| provider_from_id(&rec.id).map(str::to_string)),
        context_length: rec.context_window,
        caps: rec.caps,
    }
}

/// 内嵌记录 → 价格条目。
fn pricing_from_record(rec: &FullModelRecord) -> ModelPricing {
    ModelPricing {
        id: rec.id.clone(),
        deal: rec.deal.clone(),
        time_of_day: rec.time_of_day.clone(),
        tiers: rec.tiers.clone(),
    }
}

/// 内置列表文件里按 `pricing::find_pricing` 同款口径（精确 → 短名 → 最长前缀）查找条目。
///
/// 用于给 Provider 端点拉回的记录补齐厂商/能力等元数据：匹配偏宽松，
/// 最坏情况只是元数据不准，不影响请求路由。
fn builtin_list_entry_for(model: &str) -> Option<ModelInfo> {
    let target = model.to_ascii_lowercase();
    let entries = builtin_list_entries();
    let hit = if let Some(m) = entries.iter().find(|m| m.id.to_ascii_lowercase() == target) {
        Some(m)
    } else {
        let short = target.rsplit('/').next().unwrap_or(&target);
        let exact = entries.iter().find(|m| m.id.to_ascii_lowercase() == short);
        exact.or_else(|| {
            entries
                .iter()
                .filter(|m| short.starts_with(&m.id.to_ascii_lowercase()))
                .max_by_key(|m| m.id.len())
        })
    };
    hit.cloned()
}

/// 在给定模型列表中解析用户输入的模型名，返回条目。
///
/// 上游要求带厂商前缀的完整 ID（裸名会被拒：`Model/provider not recognized`），
/// 而部分 agent 工具对模型名长度有上限，写不下 `deepseek/deepseek-v4.1-flash`
/// 这类长 ID。故按「精确 → 短名唯一匹配」解析：
///
/// 1. 精确匹配（忽略大小写）——用户写了完整 ID 时原样命中；
/// 2. 短名匹配——把输入与各条目都去掉 `vendor/` 前缀后比对，
///    使 `deepseek-v4.1-flash` 命中 `deepseek/deepseek-v4.1-flash`；本身无前缀的
///    条目（如 `claude-sonnet-4-6`）也在这一步自然命中。
///
/// **刻意不做前缀/后缀模糊匹配**（与 `builtin_list_entry_for` 的宽松口径不同）：
/// 解析结果会作为模型 ID 发往上游，把 `deepseek-v4.1-flashx` 这类拼写错误
/// 模糊成 `deepseek-v4.1-flash` 会静默调用到另一个模型，代价远高于让上游报错。
/// 短名有歧义（多厂商同名）时同样放弃解析。
fn find_entry_in<'a>(entries: &'a [ModelInfo], model: &str) -> Option<&'a ModelInfo> {
    let target = model.trim().to_ascii_lowercase();
    if target.is_empty() {
        return None;
    }
    // 1) 完整 ID 精确匹配
    if let Some(m) = entries.iter().find(|m| m.id.to_ascii_lowercase() == target) {
        return Some(m);
    }
    // 2) 裸名（无 `vendor/` 前缀）→ 按短名唯一匹配
    if !target.contains('/') {
        let mut hits = entries
            .iter()
            .filter(|m| m.id.rsplit('/').next().unwrap_or(&m.id).to_ascii_lowercase() == target);
        let first = hits.next()?;
        // 短名在多个厂商下重复时不猜：宁可让上游报错，也不要路由到错误模型
        if hits.next().is_some() {
            return None;
        }
        return Some(first);
    }
    None
}

/// 把用户输入的模型名规范化为上游要求的完整 ID；无法解析时原样返回。
///
/// 解析依据运行时模型注册表（Provider 拉取后整表替换，含内置表兜底），
/// 匹配口径见 `find_entry_in`。
pub fn resolve_model_id(entries: &[ModelInfo], model: &str) -> String {
    match find_entry_in(entries, model) {
        Some(m) => m.id.clone(),
        None => model.to_string(),
    }
}

/// 按模型 ID 前缀推导厂商显示名：Provider 端点不带厂商字段，前缀即归属；
/// 无前缀的按模型家族识别（claude-* / gpt-*）。
pub fn provider_from_id(id: &str) -> Option<&'static str> {
    let lower = id.to_ascii_lowercase();
    if let Some((prefix, _)) = lower.split_once('/') {
        return Some(match prefix {
            "deepseek" => "DeepSeek",
            "moonshotai" => "Moonshot AI",
            "z-ai" | "zai-org" => "Z.ai",
            "minimaxai" => "MiniMax",
            "xiaomi" => "Xiaomi",
            "qwen" => "Alibaba",
            "meituan" => "Meituan",
            "stepfun" => "StepFun",
            "tencent" => "Tencent",
            "google" => "Google",
            "sakana" => "Sakana",
            "nvidia" => "NVIDIA",
            "thinkingmachines" => "Thinking Machines",
            "poolside" => "Poolside",
            "inclusionai" => "InclusionAI",
            "meta" => "Meta",
            "xai" => "xAI",
            _ => return None,
        });
    }
    if lower.starts_with("claude") {
        Some("Anthropic")
    } else if lower.starts_with("gpt") {
        Some("OpenAI")
    } else {
        None
    }
}

/// 把用户输入的模型名规范化为上游要求的完整 ID（运行时注册表版本）。
///
/// 供请求处理路径调用：直接从模型列表缓存取表，缓存为空时回退内置表，
/// 与 `/v1/models` 的取数口径一致（见 `load_models_for`）。
pub fn resolve_model_for(state: &AppState, model: &str) -> String {
    let entries = {
        let cache = state.models.read().unwrap();
        if cache.models.is_empty() {
            None
        } else {
            Some(cache.models.clone())
        }
    };
    let entries = entries.unwrap_or_else(builtin_entries);
    resolve_model_id(&entries, model)
}

/// Provider 模型列表端点的单条记录（OpenAI list 格式，字段为上游原样）。
#[derive(Debug, Clone, Deserialize)]
pub struct RemoteModel {
    pub id: String,
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub context_length: Option<u64>,
}

/// 把端点记录补全为可展示的列表条目：厂商优先按 ID 前缀推导、能力与上下文
/// 长度回退内置列表文件（端点不带能力字段；部分条目缺 context_length）。
pub fn enrich_remote(raw: RemoteModel) -> ModelInfo {
    let known = builtin_list_entry_for(&raw.id);
    let provider = provider_from_id(&raw.id)
        .map(str::to_string)
        .or_else(|| known.as_ref().and_then(|k| k.provider.clone()));
    let context_length = raw
        .context_length
        .or(known.as_ref().and_then(|k| k.context_length));
    let caps = known.map(|k| k.caps).unwrap_or_default();
    ModelInfo {
        provider,
        id: raw.id.clone(),
        name: if raw.name.is_empty() {
            raw.id
        } else {
            raw.name
        },
        context_length,
        caps,
    }
}

// ── 播种与写入 ──────────────────────────────────────────────

/// 内置列表文件的解析结果（进程内只解析一次）；文件损坏时告警并返回空表，
/// 调用方按空表跳过写入。
fn builtin_list_entries() -> Vec<ModelInfo> {
    static CACHE: OnceLock<Vec<ModelInfo>> = OnceLock::new();
    CACHE
        .get_or_init(|| {
            serde_json::from_str::<Vec<ModelInfo>>(MODELS_LIST_JSON).unwrap_or_else(|e| {
                log::warn(&format!(
                    "{}: {e}",
                    i18n::pick("内置模型列表文件解析失败", "Failed to parse the built-in model list file")
                ));
                Vec::new()
            })
        })
        .clone()
}

/// 把内置列表文件写入 `models` 表（启动时执行，整表替换）。
///
/// 文件是随版本发布的整理基线；运行中仍由 Provider 端点拉取刷新。
/// 文件解析失败时跳过写入（保留表中现有数据）。
fn sync_models_from_file(conn: &Connection) -> Result<(), String> {
    let entries = builtin_list_entries();
    if entries.is_empty() {
        return Ok(());
    }
    let n = replace_models(conn, &entries, "builtin")?;
    log::info(&format!(
        "{} {n}",
        i18n::pick(
            "内置模型列表已写入数据库，条数：",
            "Built-in model list written to the database, rows:"
        )
    ));
    Ok(())
}

/// 首次启动播种：价格表为空时写入内置价格数据，返回写入条数（非首次返回 0）。
pub fn seed_pricing_if_empty(conn: &Connection) -> Result<usize, String> {
    let pricing_empty: i64 = conn
        .query_row("SELECT COUNT(*) FROM model_pricing", [], |r| r.get(0))
        .map_err(|e| {
            let e = e.to_string();
            i18n::err_args("count_models_failed", &[&e])
        })?;
    if pricing_empty > 0 {
        return Ok(0);
    }
    let rows = pricing::builtin_pricing();
    let n = upsert_pricing(conn, &rows, "builtin")?;
    log::info(&format!(
        "{} {n}",
        i18n::pick(
            "模型价格首次落库，已写入内置数据条数：",
            "Seeded the built-in model pricing, rows written:"
        )
    ));
    Ok(n)
}

/// 用拉取到的列表整表替换 `models` 表：上游列表即权威全集，
/// 替换语义可清掉已下架的旧条目（内置列表文件与 Provider 端点同步共用此入口）。
pub fn replace_models(conn: &Connection, models: &[ModelInfo], source: &str) -> Result<usize, String> {
    let tx = conn
        .unchecked_transaction()
        .map_err(|e| {
            let e = e.to_string();
            i18n::err_args("models_tx_failed", &[&e])
        })?;
    tx.execute("DELETE FROM models", [])
        .map_err(|e| {
            let e = e.to_string();
            i18n::err_args("model_write_failed", &["*", &e])
        })?;
    let now = now_secs();
    for m in models {
        let caps = serde_json::to_string(&m.caps).map_err(|e| {
            let id = m.id.clone();
            let e = e.to_string();
            i18n::err_args("model_serialize_failed", &[&id, &e])
        })?;
        tx.execute(
            "INSERT INTO models (id, name, provider, context_length, caps, source, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            params![m.id, m.name, m.provider, m.context_length.map(|c| c as i64), caps, source, now as i64],
        )
        .map_err(|e| {
            let id = m.id.clone();
            let e = e.to_string();
            i18n::err_args("model_write_failed", &[&id, &e])
        })?;
    }
    tx.commit().map_err(|e| {
        let e = e.to_string();
        i18n::err_args("models_commit_failed", &[&e])
    })?;
    Ok(models.len())
}

/// 以覆盖语义写入模型价格：按模型 ID UPSERT，整行覆盖旧值（数据更新用）。
pub fn upsert_pricing(conn: &Connection, models: &[ModelPricing], source: &str) -> Result<usize, String> {
    let tx = conn
        .unchecked_transaction()
        .map_err(|e| {
            let e = e.to_string();
            i18n::err_args("models_tx_failed", &[&e])
        })?;
    let now = now_secs();
    for m in models {
        let tiers = serde_json::to_string(&m.tiers).map_err(|e| {
            let id = m.id.clone();
            let e = e.to_string();
            i18n::err_args("model_serialize_failed", &[&id, &e])
        })?;
        tx.execute(
            "INSERT INTO model_pricing (id, deal, tiers, time_of_day, source, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)
             ON CONFLICT(id) DO UPDATE SET
                deal = excluded.deal,
                tiers = excluded.tiers,
                time_of_day = excluded.time_of_day,
                source = excluded.source,
                updated_at = excluded.updated_at",
            params![
                m.id,
                m.deal.as_ref().and_then(|d| serde_json::to_string(d).ok()),
                tiers,
                m.time_of_day.as_ref().and_then(|t| serde_json::to_string(t).ok()),
                source,
                now as i64,
            ],
        )
        .map_err(|e| {
            let id = m.id.clone();
            let e = e.to_string();
            i18n::err_args("model_write_failed", &[&id, &e])
        })?;
    }
    tx.commit().map_err(|e| {
        let e = e.to_string();
        i18n::err_args("models_commit_failed", &[&e])
    })?;
    Ok(models.len())
}

// ── 读取 ────────────────────────────────────────────────────

/// 读取全部模型列表（按 ID 排序）。
///
/// 表为空或整表读取失败时回退内置表；个别行损坏只跳过该行并告警。
pub fn load_models(conn: &Connection) -> Vec<ModelInfo> {
    let mut out = Vec::new();
    match conn.prepare("SELECT id, name, provider, context_length, caps FROM models ORDER BY id") {
        Ok(mut stmt) => match stmt.query_map([], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, Option<String>>(2)?,
                r.get::<_, Option<i64>>(3)?,
                r.get::<_, String>(4)?,
            ))
        }) {
            Ok(rows) => {
                for row in rows.flatten() {
                    match serde_json::from_str::<ModelCaps>(&row.4) {
                        Ok(caps) => out.push(ModelInfo {
                            id: row.0,
                            name: row.1,
                            provider: row.2,
                            context_length: row.3.map(|c| c as u64),
                            caps,
                        }),
                        Err(e) => log::warn(&format!(
                            "{} {} {}: {e}",
                            i18n::pick("模型", "Model"),
                            row.0,
                            i18n::pick("数据解析失败，已跳过", "failed to parse, skipped")
                        )),
                    }
                }
            }
            Err(e) => log::warn(&format!(
                "{}: {e}",
                i18n::pick("遍历模型列表失败", "Failed to iterate the models table")
            )),
        },
        Err(e) => log::warn(&format!(
            "{}: {e}",
            i18n::pick("读取模型列表失败", "Failed to read the models table")
        )),
    }
    if out.is_empty() {
        log::warn(i18n::pick(
            "模型列表为空或不可读，回退内置兜底表",
            "Model list empty or unreadable; falling back to the built-in table",
        ));
        return builtin_entries();
    }
    out
}

/// 读取全部模型价格（按 ID 排序）；空或不可读时回退内置价格表。
pub fn load_pricing(conn: &Connection) -> Vec<ModelPricing> {
    let mut out = Vec::new();
    match conn.prepare("SELECT id, deal, tiers, time_of_day FROM model_pricing ORDER BY id") {
        Ok(mut stmt) => match stmt.query_map([], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, Option<String>>(1)?,
                r.get::<_, String>(2)?,
                r.get::<_, Option<String>>(3)?,
            ))
        }) {
            Ok(rows) => {
                for row in rows.flatten() {
                    match serde_json::from_str::<Vec<pricing::Tier>>(&row.2) {
                        Ok(tiers) => out.push(ModelPricing {
                            id: row.0,
                            deal: row
                                .1
                                .and_then(|d| serde_json::from_str::<pricing::Deal>(&d).ok()),
                            time_of_day: row
                                .3
                                .and_then(|t| serde_json::from_str::<pricing::TimeOfDay>(&t).ok()),
                            tiers,
                        }),
                        Err(e) => log::warn(&format!(
                            "{} {} {}: {e}",
                            i18n::pick("模型", "Model"),
                            row.0,
                            i18n::pick("数据解析失败，已跳过", "failed to parse, skipped")
                        )),
                    }
                }
            }
            Err(e) => log::warn(&format!(
                "{}: {e}",
                i18n::pick("遍历模型价格表失败", "Failed to iterate the model pricing table")
            )),
        },
        Err(e) => log::warn(&format!(
            "{}: {e}",
            i18n::pick("读取模型价格表失败", "Failed to read the model pricing table")
        )),
    }
    if out.is_empty() {
        log::warn(i18n::pick(
            "模型价格表为空或不可读，回退内置兜底表",
            "Model pricing table empty or unreadable; falling back to the built-in table",
        ));
        return pricing::builtin_pricing();
    }
    out
}

/// 内置列表文件的条目（模型表为空时的兜底）。
pub fn builtin_entries() -> Vec<ModelInfo> {
    builtin_list_entries()
}

/// 启动时初始化并载入价格表：建表（含旧库迁移）→ 写入内置列表 → 价格空表播种
/// → 读取价格注册表数据。
///
/// 模型列表由 Provider 端点拉取后按需刷新（见 cc_client::fetch_models），不在启动路径读取。
pub fn init_and_load(conn: &Connection) -> Result<Vec<ModelPricing>, String> {
    init_models_on(conn)?;
    sync_models_from_file(conn)?;
    seed_pricing_if_empty(conn)?;
    Ok(load_pricing(conn))
}

// ── AppState 便捷入口（供 cc_client 在拉取流程中落库 / 兜底） ──

/// 从应用状态读取模型列表；库未初始化时回退内置表。
pub fn load_models_for(state: &AppState) -> Vec<ModelInfo> {
    let guard = state.usage.lock().unwrap();
    match guard.as_ref() {
        Some(conn) => load_models(conn),
        None => builtin_entries(),
    }
}

/// 把拉取到的模型列表整表落库（source=remote）；库不可用时仅告警，不影响返回。
pub fn persist_models_for(state: &AppState, entries: &[ModelInfo]) {
    let guard = state.usage.lock().unwrap();
    if let Some(conn) = guard.as_ref() {
        match replace_models(conn, entries, "remote") {
            Ok(n) => log::info(&format!(
                "{} {n}",
                i18n::pick("模型列表已从 Provider 同步，条数：", "Model list synced from provider, rows:")
            )),
            Err(e) => log::warn(&format!(
                "{}: {e}",
                i18n::pick("模型列表落库失败", "Failed to persist the model list")
            )),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 建一个已初始化模型表的内存库（测试辅助）。
    fn temp_conn() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        init_models_on(&conn).unwrap();
        conn
    }

    /// 快捷构造一个列表条目。
    fn entry(id: &str, name: &str) -> ModelInfo {
        ModelInfo {
            id: id.into(),
            name: name.into(),
            provider: Some("Test".into()),
            context_length: Some(1000),
            caps: ModelCaps { text: true, vision: false, reasoning: true },
        }
    }

    /// 旧表迁移遇到无法解析的行：整体中止并保留旧表，避免数据永久丢失。
    #[test]
    fn legacy_migration_aborts_and_keeps_table_on_bad_row() {
        let conn = Connection::open_in_memory().unwrap();
        // 构造旧版合并表：model_pricing 带 data 列（触发迁移识别）
        conn.execute_batch(
            "CREATE TABLE model_pricing (id TEXT PRIMARY KEY, data TEXT NOT NULL, source TEXT NOT NULL);",
        )
        .unwrap();
        let good = r#"{"id":"m-good","name":"Good","tiers":[]}"#;
        conn.execute(
            "INSERT INTO model_pricing (id, data, source) VALUES ('m-good', ?1, 'builtin')",
            params![good],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO model_pricing (id, data, source) VALUES ('m-bad', 'not json', 'builtin')",
            [],
        )
        .unwrap();

        // 迁移应失败而非静默跳过坏行
        assert!(init_models_on(&conn).is_err());
        // 事务整体回滚：原 model_pricing 表（含 data 列与全部行）保留，可稍后重试
        let legacy_rows: i64 = conn
            .query_row("SELECT COUNT(*) FROM model_pricing", [], |r| r.get(0))
            .unwrap();
        assert_eq!(legacy_rows, 2, "迁移中止后旧表数据应完整保留");
        // 回滚后旧表仍是带 data 列的形态，下次启动会重新尝试迁移
        assert!(legacy_table_exists(&conn).unwrap());
    }

    /// 快捷构造一个带单档费率的价格条目。
    fn sample(id: &str) -> ModelPricing {
        ModelPricing {
            id: id.into(),
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

    /// 价格播种仅在空表时生效，重复播种不覆盖。
    #[test]
    fn seed_pricing_only_when_empty() {
        let conn = temp_conn();
        let n = seed_pricing_if_empty(&conn).unwrap();
        assert_eq!(n, pricing::builtin_pricing().len());
        // 再次播种不写入、不覆盖
        assert_eq!(seed_pricing_if_empty(&conn).unwrap(), 0);
        assert_eq!(load_pricing(&conn).len(), pricing::builtin_pricing().len());
    }

    /// 启动同步：内置列表文件整表写入 models 表，重复执行结果一致。
    #[test]
    fn sync_models_from_file_writes_baseline() {
        let conn = temp_conn();
        // 模拟旧数据残留：同步前先写入一条无关行，应被整表替换清除
        replace_models(&conn, &[entry("stale-x", "残留")], "manual").unwrap();
        init_and_load(&conn).unwrap();
        let rows = load_models(&conn);
        assert!(!rows.is_empty(), "内置列表文件应写入数据");
        assert!(rows.iter().all(|m| m.id != "stale-x"), "整表替换应清除残留行");
        assert!(rows.iter().any(|m| m.id == "tencent/hy3-paid"), "应包含整理基线中的条目");
        // 再执行一次结果一致（幂等）
        init_and_load(&conn).unwrap();
        assert_eq!(load_models(&conn).len(), rows.len());
        let source: String = conn
            .query_row("SELECT source FROM models LIMIT 1", [], |r| r.get(0))
            .unwrap();
        assert_eq!(source, "builtin");
    }

    /// 价格 UPSERT 覆盖同 ID 旧行（费率与来源均更新）。
    #[test]
    fn upsert_pricing_overwrites_existing_row() {
        let conn = temp_conn();
        upsert_pricing(&conn, &[sample("m1")], "manual").unwrap();
        let mut newer = sample("m1");
        newer.tiers[0].rates.input = 9.0;
        upsert_pricing(&conn, &[newer], "remote").unwrap();
        let rows = load_pricing(&conn);
        assert_eq!(rows.len(), 1);
        assert!((rows[0].tiers[0].rates.input - 9.0).abs() < 1e-9);
        let source: String = conn
            .query_row("SELECT source FROM model_pricing WHERE id='m1'", [], |r| r.get(0))
            .unwrap();
        assert_eq!(source, "remote");
    }

    /// 整表替换清掉未在新列表中的旧条目（上游下架语义）。
    #[test]
    fn replace_models_removes_stale_rows() {
        let conn = temp_conn();
        replace_models(&conn, &[entry("stale", "旧条目")], "builtin").unwrap();
        replace_models(&conn, &[entry("fresh", "新条目")], "remote").unwrap();
        let rows = load_models(&conn);
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].id, "fresh");
    }

    /// init_and_load 先播种再读库：新增自定义价格行后能被读出（证明读的是数据库）。
    #[test]
    fn init_and_load_seeds_then_reads_db() {
        let conn = temp_conn();
        let rows = init_and_load(&conn).unwrap();
        assert!(rows.len() >= 60, "播种后应拿到完整内置价格表: {}", rows.len());
        upsert_pricing(&conn, &[sample("custom-x")], "manual").unwrap();
        let rows = init_and_load(&conn).unwrap();
        assert!(rows.iter().any(|m| m.id == "custom-x"));
    }

    /// 损坏的价格行被跳过而不影响其他行的加载。
    #[test]
    fn bad_row_is_skipped_not_fatal() {
        let conn = temp_conn();
        conn.execute(
            "INSERT INTO model_pricing (id, deal, tiers, time_of_day, source, updated_at)
             VALUES ('bad', NULL, '{not json', NULL, 'manual', 0)",
            [],
        )
        .unwrap();
        upsert_pricing(&conn, &[sample("good")], "manual").unwrap();
        let rows = load_pricing(&conn);
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].id, "good");
    }

    /// 促销模型的标牌价（listRates）经落库再读出后原样保留。
    #[test]
    fn persisted_json_round_trips_list_rates() {
        let conn = temp_conn();
        let mut m = sample("promo");
        m.tiers[0].list_rates =
            Some(pricing::Rates { input: 5.0, output: 15.0, cached: 1.0, cache_write: 6.26 });
        upsert_pricing(&conn, &[m], "manual").unwrap();

        let back = &load_pricing(&conn)[0];
        let list = back.tiers[0].list_rates.expect("listRates 应被保留");
        assert!((list.input - 5.0).abs() < 1e-9);
        assert!((list.output - 15.0).abs() < 1e-9);
        assert!((list.cache_write - 6.26).abs() < 1e-9);
    }

    /// 旧版合并表（data JSON 同时存列表与价格）启动时自动拆分迁移到两张新表。
    #[test]
    fn legacy_combined_table_is_migrated() {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE model_pricing (
                id TEXT PRIMARY KEY,
                data TEXT NOT NULL,
                source TEXT NOT NULL,
                updated_at INTEGER NOT NULL
            );",
        )
        .unwrap();
        // 手工构造一条旧版合并记录（含列表与价格字段）
        let rec = FullModelRecord {
            id: "legacy-model".into(),
            name: "Legacy Model".into(),
            provider: Some("Legacy".into()),
            context_window: Some(1234),
            caps: ModelCaps { text: true, vision: false, reasoning: true },
            deal: None,
            time_of_day: None,
            tiers: vec![pricing::Tier {
                max_context: None,
                rates: pricing::Rates { input: 1.0, output: 2.0, cached: 0.1, cache_write: 0.0 },
                list_rates: None,
            }],
        };
        let data = serde_json::to_string(&serde_json::json!({
            "id": rec.id,
            "name": rec.name,
            "category": "opensource",
            "provider": rec.provider,
            "contextWindow": rec.context_window,
            "caps": rec.caps,
            "tiers": rec.tiers,
            "deal": rec.deal,
            "timeOfDay": rec.time_of_day,
        }))
        .unwrap();
        conn.execute(
            "INSERT INTO model_pricing (id, data, source, updated_at) VALUES (?1, ?2, 'builtin', 0)",
            params![rec.id, data],
        )
        .unwrap();

        // 走建表入口触发迁移
        init_models_on(&conn).unwrap();
        // 旧表已删除，双表就位且数据拆分正确
        let models = load_models(&conn);
        assert_eq!(models.len(), 1);
        assert_eq!(models[0].id, rec.id);
        assert_eq!(models[0].provider, rec.provider);
        assert_eq!(models[0].context_length, rec.context_window);
        let pricing = load_pricing(&conn);
        assert_eq!(pricing.len(), 1);
        assert_eq!(pricing[0].tiers.len(), rec.tiers.len());
        let legacy: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name='model_pricing_legacy'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(legacy, 0, "迁移完成后旧表备份应被删除");
    }

    /// 走启动同一条路径（init_usage 建库后初始化）在真实数据库文件上工作。
    #[test]
    fn works_on_real_database_file() {
        let path = std::env::temp_dir().join(format!("cc-models-{}.sqlite", std::process::id()));
        let _ = std::fs::remove_file(&path);
        let conn = super::super::usage::init_usage(&path).unwrap();
        let pricing_rows = init_and_load(&conn).unwrap();
        assert!(
            pricing_rows.len() >= 60,
            "首次启动应播种完整内置价格表: {}",
            pricing_rows.len()
        );
        assert!(load_models(&conn).len() >= 60, "模型列表应同步播种");
        // 用量表与模型表共存于同一库
        let cnt: i64 = conn
            .query_row("SELECT COUNT(*) FROM usage_history", [], |r| r.get(0))
            .unwrap();
        assert_eq!(cnt, 0);
        // 播种是无损的：内置常量里的标牌价（qwen-3.7-max 带 listRates）应原样落库
        let promo = pricing_rows.iter().find(|m| m.id == "qwen-3.7-max").unwrap();
        assert!(promo.tiers[0].list_rates.is_some(), "播种后不应丢失 listRates");
        let _ = std::fs::remove_file(&path);
    }

    /// 模型表被删除（异常环境）时：播种报错、load 回退内置数据而非 panic。
    #[test]
    fn table_missing_degrades_gracefully() {
        let conn = temp_conn();
        conn.execute("DROP TABLE model_pricing", []).unwrap();
        assert!(seed_pricing_if_empty(&conn).is_err());
        assert_eq!(load_pricing(&conn).len(), pricing::builtin_pricing().len());
        assert_eq!(load_models(&conn).len(), builtin_entries().len());
    }

    /// 内置列表文件（models.json）的数据质量：能力标记随文件可读。
    #[test]
    fn builtin_list_file_carries_caps() {
        let entries = builtin_entries();
        assert!(entries.len() >= 60, "内置列表条目过少: {}", entries.len());
        let k = entries.iter().find(|m| m.id == "moonshotai/Kimi-K3").unwrap();
        assert!(k.caps.text && k.caps.vision && k.caps.reasoning);
        let m = entries.iter().find(|m| m.id == "MiniMaxAI/MiniMax-M2.5").unwrap();
        assert!(!m.caps.reasoning);
    }

    /// 厂商前缀推导：常见前缀、claude/gpt 家族与未知前缀。
    #[test]
    fn provider_prefix_mapping() {
        assert_eq!(provider_from_id("meituan/LongCat-2.0:free"), Some("Meituan"));
        assert_eq!(provider_from_id("tencent/hy3-paid"), Some("Tencent"));
        assert_eq!(provider_from_id("zai-org/GLM-5.3"), Some("Z.ai"));
        assert_eq!(provider_from_id("z-ai/glm-5.3-flash"), Some("Z.ai"));
        assert_eq!(provider_from_id("Qwen/Qwen3.8-Max"), Some("Alibaba"));
        assert_eq!(provider_from_id("claude-opus-5"), Some("Anthropic"));
        assert_eq!(provider_from_id("gpt-5.5"), Some("OpenAI"));
        assert_eq!(provider_from_id("unknown-vendor/thing"), None);
        assert_eq!(provider_from_id("mystery-model"), None);
    }

    /// 模型名解析：裸名补齐厂商前缀，完整 ID 原样保留，拼错/歧义不猜。
    #[test]
    fn resolve_model_id_fills_vendor_prefix() {
        let entries = builtin_entries();
        // 裸名 → 补上厂商前缀（上游只认完整 ID）
        assert_eq!(
            resolve_model_id(&entries, "deepseek-v4.1-flash"),
            "deepseek/deepseek-v4.1-flash"
        );
        assert_eq!(
            resolve_model_id(&entries, "muse-spark-1.3-contributor"),
            "meta/muse-spark-1.3-contributor"
        );
        assert_eq!(
            resolve_model_id(&entries, "Kimi-K3"),
            "moonshotai/Kimi-K3"
        );
        // 完整 ID 原样保留（含大小写差异，匹配不区分大小写但输出取注册表写法）
        assert_eq!(
            resolve_model_id(&entries, "deepseek/deepseek-v4.1-flash"),
            "deepseek/deepseek-v4.1-flash"
        );
        // 本身无前缀的模型：裸名即完整 ID
        assert_eq!(resolve_model_id(&entries, "claude-sonnet-4-6"), "claude-sonnet-4-6");
        // 首尾空白容忍
        assert_eq!(
            resolve_model_id(&entries, "  deepseek-v4.1-flash  "),
            "deepseek/deepseek-v4.1-flash"
        );

        // 未收录的模型原样返回，交由上游报错（不猜测、不模糊匹配）
        assert_eq!(
            resolve_model_id(&entries, "deepseek-v4.1-flashX"),
            "deepseek-v4.1-flashX",
            "拼写相近但不是已知模型时不得模糊成另一个模型"
        );
        assert_eq!(resolve_model_id(&entries, "totally-unknown"), "totally-unknown");
        assert_eq!(resolve_model_id(&entries, ""), "");
    }

    /// 短名在多个厂商下重复时必须放弃解析，避免静默路由到错误厂商。
    #[test]
    fn resolve_model_id_refuses_ambiguous_short_name() {
        let entries = vec![
            ModelInfo {
                id: "vendor-a/shared-name".into(),
                name: "A".into(),
                provider: None,
                context_length: None,
                caps: ModelCaps::default(),
            },
            ModelInfo {
                id: "vendor-b/shared-name".into(),
                name: "B".into(),
                provider: None,
                context_length: None,
                caps: ModelCaps::default(),
            },
        ];
        assert_eq!(
            resolve_model_id(&entries, "shared-name"),
            "shared-name",
            "歧义短名应原样透传而非任选一个"
        );
        // 写全 ID 时两个都能正确命中
        assert_eq!(resolve_model_id(&entries, "vendor-a/shared-name"), "vendor-a/shared-name");
        assert_eq!(resolve_model_id(&entries, "vendor-b/shared-name"), "vendor-b/shared-name");
    }

    /// 端点记录补全：厂商取前缀推导，能力/上下文回退内置表，缺失名称回退 ID。
    #[test]
    fn enrich_remote_fills_provider_and_caps() {
        // 带前缀的已知模型：厂商取前缀，能力与上下文回退内置表
        let e = enrich_remote(RemoteModel {
            id: "meituan/LongCat-2.0:free".into(),
            name: "LongCat 2.0".into(),
            context_length: None,
        });
        assert_eq!(e.provider.as_deref(), Some("Meituan"));
        assert_eq!(e.context_length, Some(1_048_576));
        assert!(!e.caps.vision);
        // 无前缀、无名称：回退 ID 本身，能力为默认值
        let e = enrich_remote(RemoteModel {
            id: "mock-model-1".into(),
            name: String::new(),
            context_length: Some(123),
        });
        assert_eq!(e.name, "mock-model-1");
        assert_eq!(e.provider, None);
        assert_eq!(e.context_length, Some(123));
        assert!(!e.caps.text);
    }
}
