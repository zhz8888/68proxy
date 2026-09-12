//! token 用量统计的数据层：SQLite 持久化（usage_history 明细、usage_daily 按天聚合、_meta 元数据）。
//!
//! - `usage_history`：每次请求一条明细（模型、端点、状态、各 token 列与估算成本）。
//! - `usage_daily`：按本地时区「天」预聚合的 JSON，支撑 7D/30D/60D 大时间窗快速查询。
//! - `usage_meta`：生命周期计数（总请求数）。
//!
//! 同一数据库文件还承载设置表（settings）与模型信息表（model_pricing），
//! 建表统一在此处的 `init_usage_on` 完成，具体读写归属各自模块。
//!
//! 采集挂载点位于代理请求处理管线（server.rs），此处只负责建库、写入与查询。

use chrono::{Datelike, Local, Timelike};
use rusqlite::{params, Connection, OptionalExtension};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::path::Path;
use std::sync::Mutex;

use super::pricing;
use crate::i18n;

/// 用量更新事件转发器（Tauri 注入，供前端统计页实时刷新）。
static USAGE_SINK: Mutex<Option<Box<dyn Fn() + Send + Sync>>> = Mutex::new(None);

/// 注册用量更新回调（Tauri setup 阶段调用，节流由调用方负责）。
pub fn set_usage_sink<F>(f: F)
where
    F: Fn() + Send + Sync + 'static,
{
    *USAGE_SINK.lock().unwrap() = Some(Box::new(f));
}

/// 用量记录成功后触发事件回调（无注册时静默）。
fn emit_usage_updated() {
    if let Some(f) = USAGE_SINK.lock().unwrap().as_ref() {
        f();
    }
}

/// 一次请求的 token 用量记录（写入前由 server.rs 组装）。
#[derive(Debug, Clone)]
pub struct UsageEntry {
    /// 请求开始时间（Unix 毫秒）。
    pub ts: u64,
    /// 请求的模型名。
    pub model: String,
    /// 入口端点（/v1/chat/completions 等）。
    pub endpoint: String,
    /// 请求状态：ok / error / timeout / disconnect。
    pub status: String,
    /// 输入 token（含缓存命中与缓存写入）。
    pub prompt_tokens: u64,
    /// 输出 token。
    pub completion_tokens: u64,
    /// 命中缓存的输入 token 数（prompt 子集）。
    pub cached_tokens: u64,
    /// 写入缓存的输入 token 数（prompt 子集）。
    pub cache_write_tokens: u64,
    /// 是否流式请求。
    pub stream: bool,
}

/// 汇总统计结果（对应 9router 的 getUsageStats 返回结构）。
#[derive(Debug, Serialize)]
pub struct UsageStats {
    pub total_requests: u64,
    pub total_prompt_tokens: u64,
    pub total_completion_tokens: u64,
    pub total_cached_tokens: u64,
    pub total_cost: f64,
    /// 按模型分组（含请求数、各 token 列与成本）。
    pub by_model: Vec<GroupRow>,
    /// 按端点分组。
    pub by_endpoint: Vec<GroupRow>,
    /// 最近 10 分钟（10 个分钟桶，数组按时间从旧到新排列，index 0 最早）。
    pub last_10_minutes: Vec<MinuteBucket>,
    /// 最近请求明细（新在前，上限 20 条）。
    pub recent_requests: Vec<RecentRow>,
}

/// 分组统计行。
#[derive(Debug, Serialize)]
pub struct GroupRow {
    pub key: String,
    pub requests: u64,
    pub prompt_tokens: u64,
    pub completion_tokens: u64,
    pub cached_tokens: u64,
    pub total_tokens: u64,
    pub cost: f64,
}

/// 分钟桶。
#[derive(Debug, Serialize)]
pub struct MinuteBucket {
    pub requests: u64,
    pub prompt_tokens: u64,
    pub completion_tokens: u64,
    pub cost: f64,
}

/// 最近请求明细行。
#[derive(Debug, Serialize)]
pub struct RecentRow {
    pub ts: u64,
    pub model: String,
    pub endpoint: String,
    pub status: String,
    pub prompt_tokens: u64,
    pub completion_tokens: u64,
    pub cached_tokens: u64,
    pub cache_write_tokens: u64,
    pub cost: f64,
    pub elapsed_ms: u64,
}

/// 趋势图数据点（对应 9router 的 getChartData）。
#[derive(Debug, Serialize)]
pub struct ChartPoint {
    /// 桶标签：小时桶为 "HH:00"，天桶为 "MM-DD"。
    pub label: String,
    pub prompt_tokens: u64,
    pub completion_tokens: u64,
    pub cost: f64,
}

/// 按天预聚合的 JSON 结构（usage_daily.data）。
#[derive(Debug, Default, Clone, Serialize, Deserialize)]
struct DayAgg {
    requests: u64,
    prompt_tokens: u64,
    completion_tokens: u64,
    cached_tokens: u64,
    cost: f64,
    /// 模型ID -> 聚合行。
    by_model: HashMap<String, DayRow>,
    /// 端点 -> 聚合行。
    by_endpoint: HashMap<String, DayRow>,
}

/// 分组维度在 DayAgg 中的聚合行。
#[derive(Debug, Default, Clone, Serialize, Deserialize)]
struct DayRow {
    requests: u64,
    prompt_tokens: u64,
    completion_tokens: u64,
    cached_tokens: u64,
    cost: f64,
}

/// 时间范围。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Period {
    /// 今日（从本地 0 点起）。
    Today,
    /// 最近 24 小时。
    H24,
    /// 最近 7 天。
    D7,
    /// 最近 30 天。
    D30,
    /// 最近 60 天。
    D60,
    /// 全部。
    All,
}

impl Period {
    /// 解析命令入参字符串，非法值回退全部。
    pub fn parse(s: &str) -> Self {
        match s {
            "today" => Period::Today,
            "24h" => Period::H24,
            "7d" => Period::D7,
            "30d" => Period::D30,
            "60d" => Period::D60,
            _ => Period::All,
        }
    }

    /// 返回需要按天预聚合的最大天数（today/24h 返回 None，实时扫明细表）。
    pub fn days(&self) -> Option<u32> {
        match self {
            Period::Today | Period::H24 => None,
            Period::D7 => Some(7),
            Period::D30 => Some(30),
            Period::D60 => Some(60),
            Period::All => None,
        }
    }
}

/// 初始化数据库：建表、索引并设置 PRAGMA。失败返回中文错误描述。
pub fn init_usage(path: &Path) -> Result<Connection, String> {
    let conn = Connection::open(path).map_err(|e| {
        let e = e.to_string();
        i18n::err_args("open_usage_db_failed", &[&e])
    })?;
    init_usage_on(&conn)?;
    Ok(conn)
}

/// 在已有连接上建表与索引（供 init_usage 与内存库测试复用）。
pub fn init_usage_on(conn: &Connection) -> Result<(), String> {
    super::settings::init_settings_on(conn)?;
    super::models::init_models_on(conn)?;
    conn.pragma_update(None, "journal_mode", "WAL")
        .map_err(|e| {
            let e = e.to_string();
            i18n::err_args("wal_failed", &[&e])
        })?;
    conn.pragma_update(None, "synchronous", "NORMAL").ok();
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS usage_history (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            ts INTEGER NOT NULL,
            model TEXT NOT NULL,
            endpoint TEXT NOT NULL,
            status TEXT NOT NULL,
            prompt_tokens INTEGER NOT NULL DEFAULT 0,
            completion_tokens INTEGER NOT NULL DEFAULT 0,
            cached_tokens INTEGER NOT NULL DEFAULT 0,
            cache_write_tokens INTEGER NOT NULL DEFAULT 0,
            cost REAL NOT NULL DEFAULT 0,
            stream INTEGER NOT NULL DEFAULT 0
        );
        CREATE INDEX IF NOT EXISTS idx_uh_ts ON usage_history(ts);
        CREATE INDEX IF NOT EXISTS idx_uh_model ON usage_history(model);
        CREATE TABLE IF NOT EXISTS usage_daily (
            date_key TEXT PRIMARY KEY,
            data TEXT NOT NULL
        );
        CREATE TABLE IF NOT EXISTS usage_meta (
            key TEXT PRIMARY KEY,
            value TEXT NOT NULL
        );",
    )
    .map_err(|e| {
        let e = e.to_string();
        i18n::err_args("init_usage_table_failed", &[&e])
    })?;
    Ok(())
}

/// 打开已存在的用量数据库（只读查询用）；文件缺失时返回错误。
pub fn open_usage(path: &Path) -> Result<Connection, String> {
    Connection::open(path).map_err(|e| {
        let e = e.to_string();
        i18n::err_args("open_usage_db_failed", &[&e])
    })
}

/// 把 Unix 毫秒时间戳转为本地时区的天键（YYYY-MM-DD）。
fn day_key_of(ts: u64) -> String {
    let secs = ts as i64 / 1000;
    let d = chrono::DateTime::from_timestamp(secs, 0)
        .map(|dt| dt.with_timezone(&Local))
        .unwrap_or_else(Local::now);
    format!("{:04}-{:02}-{:02}", d.year(), d.month(), d.day())
}

/// 把 Unix 毫秒时间戳转为本地时区的小时标签（HH:00）。
fn hour_label_of(ts: u64) -> String {
    let secs = ts as i64 / 1000;
    let d = chrono::DateTime::from_timestamp(secs, 0)
        .map(|dt| dt.with_timezone(&Local))
        .unwrap_or_else(Local::now);
    format!("{:02}:00", d.hour())
}

/// 今天本地 0 点对应的 Unix 毫秒时间戳。
///
/// DST 跳变时午夜可能是「不存在」或「有歧义」的时刻，`.single()` 会返回 None；
/// 此时回退 `earliest()`（跳到最早的有效时刻），**不能**回退 0，
/// 否则「今日」统计会退化为统计全部历史。
fn local_midnight_millis() -> u64 {
    let n = chrono::Local::now();
    let midnight = n
        .date_naive()
        .and_hms_opt(0, 0, 0)
        .unwrap_or_else(|| n.date_naive().and_hms_opt(0, 0, 1).unwrap());
    midnight
        .and_local_timezone(Local)
        .earliest()
        .map(|dt| dt.timestamp_millis() as u64)
        .unwrap_or_else(|| {
            // 极端兜底：取当天 0 点前推 1 毫秒所在时刻，保证为「今日起点」量级
            n.timestamp_millis().max(0) as u64
        })
}

/// 读取某天的预聚合数据，不存在时返回默认空结构。
fn load_day(conn: &Connection, key: &str) -> DayAgg {
    let raw: Option<String> = conn
        .query_row(
            "SELECT data FROM usage_daily WHERE date_key = ?1",
            params![key],
            |r| r.get(0),
        )
        .optional()
        .unwrap_or(None);
    raw.and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default()
}

/// 写入某天的预聚合数据。
fn save_day(conn: &Connection, key: &str, agg: &DayAgg) -> Result<(), String> {
    let data = serde_json::to_string(agg).map_err(|e| {
        let e = e.to_string();
        i18n::err_args("aggregate_serialize_failed", &[&e])
    })?;
    conn.execute(
        "INSERT INTO usage_daily (date_key, data) VALUES (?1, ?2)
         ON CONFLICT(date_key) DO UPDATE SET data = excluded.data",
        params![key, data],
    )
    .map_err(|e| {
        format!(
            "{}: {e}",
            i18n::pick("写入按天聚合失败", "Failed to write daily aggregate")
        )
    })?;
    Ok(())
}

/// 记录一次请求的用量：写明细 + 更新按天聚合 + 自增生命周期计数。
///
/// 三个写入包在单个事务内，保证崩溃时三者一致（明细 / 聚合 / 计数要么全落盘要么全不落），
/// 并减少提交次数。写入成本按当前单价表实时估算。
pub fn record_usage(conn: &Connection, entry: &UsageEntry) -> Result<(), String> {
    let cost = pricing::calculate_cost(
        &entry.model,
        entry.prompt_tokens,
        entry.completion_tokens,
        entry.cached_tokens,
        entry.cache_write_tokens,
        entry.ts,
    );
    let tx = conn
        .unchecked_transaction()
        .map_err(|e| {
            format!(
                "{}: {e}",
                i18n::pick("开启用量事务失败", "Failed to begin the usage transaction")
            )
        })?;
    conn.execute(
        "INSERT INTO usage_history
            (ts, model, endpoint, status, prompt_tokens, completion_tokens, cached_tokens, cache_write_tokens, cost, stream)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
        params![
            entry.ts as i64,
            entry.model,
            entry.endpoint,
            entry.status,
            entry.prompt_tokens as i64,
            entry.completion_tokens as i64,
            entry.cached_tokens as i64,
            entry.cache_write_tokens as i64,
            cost,
            entry.stream as i64,
        ],
    )
    .map_err(|e| {
        format!(
            "{}: {e}",
            i18n::pick("写入用量明细失败", "Failed to write usage row")
        )
    })?;

    // 更新按天聚合
    let key = day_key_of(entry.ts);
    let mut agg = load_day(conn, &key);
    agg.requests += 1;
    agg.prompt_tokens += entry.prompt_tokens;
    agg.completion_tokens += entry.completion_tokens;
    agg.cached_tokens += entry.cached_tokens;
    agg.cost += cost;
    let row = agg.by_model.entry(entry.model.clone()).or_default();
    row.requests += 1;
    row.prompt_tokens += entry.prompt_tokens;
    row.completion_tokens += entry.completion_tokens;
    row.cached_tokens += entry.cached_tokens;
    row.cost += cost;
    let row = agg.by_endpoint.entry(entry.endpoint.clone()).or_default();
    row.requests += 1;
    row.prompt_tokens += entry.prompt_tokens;
    row.completion_tokens += entry.completion_tokens;
    row.cached_tokens += entry.cached_tokens;
    row.cost += cost;
    save_day(conn, &key, &agg)?;

    // 生命周期总请求数（原子自增，读改写放在单条 SQL 内避免并发竞态）
    conn.execute(
        "INSERT INTO usage_meta (key, value) VALUES ('total_requests_lifetime', '1')
         ON CONFLICT(key) DO UPDATE SET value = CAST(value AS INTEGER) + 1",
        [],
    )
    .map_err(|e| {
        format!(
            "{}: {e}",
            i18n::pick("更新生命周期计数失败", "Failed to update the lifetime counter")
        )
    })?;
    tx.commit().map_err(|e| {
        format!(
            "{}: {e}",
            i18n::pick("提交用量事务失败", "Failed to commit the usage transaction")
        )
    })?;
    emit_usage_updated();
    Ok(())
}

/// 把 usage_daily 的某天数据并入汇总聚合结果（供 7D/30D/60D 快速统计）。
fn merge_day_into(
    total: &mut DayAgg,
    start_day: &str,
    end_day: &str,
    conn: &Connection,
) -> Result<(), String> {
    let mut stmt = conn
        .prepare("SELECT date_key, data FROM usage_daily WHERE date_key >= ?1 AND date_key <= ?2")
        .map_err(|e| {
            let e = e.to_string();
            i18n::err_args("query_daily_failed", &[&e])
        })?;
    let rows = stmt
        .query_map(params![start_day, end_day], |r| {
            Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))
        })
        .map_err(|e| {
            let e = e.to_string();
            i18n::err_args("iterate_daily_failed", &[&e])
        })?;
    for row in rows {
        let (_, data) = row.map_err(|e| {
            let e = e.to_string();
            i18n::err_args("read_daily_failed", &[&e])
        })?;
        if let Ok(agg) = serde_json::from_str::<DayAgg>(&data) {
            total.requests += agg.requests;
            total.prompt_tokens += agg.prompt_tokens;
            total.completion_tokens += agg.completion_tokens;
            total.cached_tokens += agg.cached_tokens;
            total.cost += agg.cost;
            for (k, v) in agg.by_model {
                let r = total.by_model.entry(k).or_default();
                r.requests += v.requests;
                r.prompt_tokens += v.prompt_tokens;
                r.completion_tokens += v.completion_tokens;
                r.cached_tokens += v.cached_tokens;
                r.cost += v.cost;
            }
            for (k, v) in agg.by_endpoint {
                let r = total.by_endpoint.entry(k).or_default();
                r.requests += v.requests;
                r.prompt_tokens += v.prompt_tokens;
                r.completion_tokens += v.completion_tokens;
                r.cached_tokens += v.cached_tokens;
                r.cost += v.cost;
            }
        }
    }
    Ok(())
}

/// 汇总统计：按时间范围聚合明细或按天预聚合数据，得到汇总卡片与分组表。
pub fn get_stats(conn: &Connection, period: Period) -> Result<UsageStats, String> {
    let now = super::state::now_millis();
    let mut total = DayAgg::default();

    match period.days() {
        // 7D/30D/60D：从按天预聚合读取
        Some(days) => {
            let start = chrono::Local::now() - chrono::Duration::days(days as i64 - 1);
            let start_key = format!("{:04}-{:02}-{:02}", start.year(), start.month(), start.day());
            let end = chrono::Local::now();
            let end_key = format!("{:04}-{:02}-{:02}", end.year(), end.month(), end.day());
            merge_day_into(&mut total, &start_key, &end_key, conn)?;
        }
        // Today：从本地 0 点起；24h：从 now-24h 起 —— 均实时扫明细
        None => {
            let cutoff = match period {
                Period::Today => local_midnight_millis(),
                Period::H24 => now.saturating_sub(24 * 60 * 60 * 1000),
                _ => 0,
            };
            let rows = query_rows(conn, cutoff, i64::MAX as u64)?;
            for r in rows {
                // 成本按明细的 token 规模与发生时刻重新估算，使分档计费与闲/忙时费率生效
                let cost = super::pricing::calculate_cost(
                    &r.model,
                    r.prompt_tokens,
                    r.completion_tokens,
                    r.cached_tokens,
                    r.cache_write_tokens,
                    r.ts,
                );
                total.requests += 1;
                total.prompt_tokens += r.prompt_tokens;
                total.completion_tokens += r.completion_tokens;
                total.cached_tokens += r.cached_tokens;
                total.cost += cost;
                let row = total.by_model.entry(r.model.clone()).or_default();
                row.requests += 1;
                row.prompt_tokens += r.prompt_tokens;
                row.completion_tokens += r.completion_tokens;
                row.cached_tokens += r.cached_tokens;
                row.cost += cost;
                let row = total.by_endpoint.entry(r.endpoint.clone()).or_default();
                row.requests += 1;
                row.prompt_tokens += r.prompt_tokens;
                row.completion_tokens += r.completion_tokens;
                row.cached_tokens += r.cached_tokens;
                row.cost += cost;
            }
        }
    }

    // 组装分组行（按请求数降序）
    let mut by_model: Vec<GroupRow> = total
        .by_model
        .into_iter()
        .map(|(key, v)| GroupRow {
            key,
            requests: v.requests,
            prompt_tokens: v.prompt_tokens,
            completion_tokens: v.completion_tokens,
            cached_tokens: v.cached_tokens,
            total_tokens: v.prompt_tokens + v.completion_tokens,
            cost: v.cost,
        })
        .collect();
    by_model.sort_by(|a, b| b.requests.cmp(&a.requests));

    let mut by_endpoint: Vec<GroupRow> = total
        .by_endpoint
        .into_iter()
        .map(|(key, v)| GroupRow {
            key,
            requests: v.requests,
            prompt_tokens: v.prompt_tokens,
            completion_tokens: v.completion_tokens,
            cached_tokens: v.cached_tokens,
            total_tokens: v.prompt_tokens + v.completion_tokens,
            cost: v.cost,
        })
        .collect();
    by_endpoint.sort_by(|a, b| b.requests.cmp(&a.requests));

    // 最近 10 分钟：10 个分钟桶（数组按时间从旧到新排列，index 0 最早）
    let mut last_10_minutes = Vec::with_capacity(10);
    let min_cutoff = now.saturating_sub(10 * 60 * 1000);
    let recent = query_rows(conn, min_cutoff, i64::MAX as u64)?;
    for i in 0..10u64 {
        let bucket_start = now.saturating_sub((9 - i + 1) * 60 * 1000);
        let bucket_end = now.saturating_sub((9 - i) * 60 * 1000);
        let mut b = MinuteBucket {
            requests: 0,
            prompt_tokens: 0,
            completion_tokens: 0,
            cost: 0.0,
        };
        for r in &recent {
            if r.ts >= bucket_start && r.ts < bucket_end {
                b.requests += 1;
                b.prompt_tokens += r.prompt_tokens;
                b.completion_tokens += r.completion_tokens;
                b.cost += super::pricing::calculate_cost(
                    &r.model,
                    r.prompt_tokens,
                    r.completion_tokens,
                    r.cached_tokens,
                    r.cache_write_tokens,
                    r.ts,
                );
            }
        }
        last_10_minutes.push(b);
    }

    // 最近 20 条明细（全时间窗内最新）
    let recent_requests = query_recent(conn, 20)?;

    Ok(UsageStats {
        total_requests: total.requests,
        total_prompt_tokens: total.prompt_tokens,
        total_completion_tokens: total.completion_tokens,
        total_cached_tokens: total.cached_tokens,
        total_cost: total.cost,
        by_model,
        by_endpoint,
        last_10_minutes,
        recent_requests,
    })
}

/// 趋势图数据：today/24h 按小时 24 桶，7D/30D/60D 按天 N 桶
/// （数组按时间从旧到新排列，index 0 最早）。
pub fn get_chart(conn: &Connection, period: Period) -> Result<Vec<ChartPoint>, String> {
    let now = super::state::now_millis();
    match period.days() {
        Some(days) => {
            // 按天：从 usage_daily 读取每天合计
            let mut points = Vec::new();
            for i in (0..days).rev() {
                let d = chrono::Local::now() - chrono::Duration::days(i as i64);
                let key = format!("{:04}-{:02}-{:02}", d.year(), d.month(), d.day());
                let agg = load_day(conn, &key);
                points.push(ChartPoint {
                    label: format!("{:02}-{:02}", d.month(), d.day()),
                    prompt_tokens: agg.prompt_tokens,
                    completion_tokens: agg.completion_tokens,
                    cost: agg.cost,
                });
            }
            Ok(points)
        }
        None => {
            let cutoff = match period {
                Period::Today => local_midnight_millis(),
                _ => now.saturating_sub(24 * 60 * 60 * 1000),
            };
            let rows = query_rows(conn, cutoff, i64::MAX as u64)?;
            // 24 个 1 小时桶（数组按时间从旧到新排列，index 0 最早）
            let mut buckets = vec![(0u64, 0u64, 0.0f64); 24];
            for r in &rows {
                let idx = (now.saturating_sub(r.ts)) / (60 * 60 * 1000);
                let idx = (24 - 1 - idx.min(23) as usize) as usize; // 旧桶 idx 小
                if idx < 24 {
                    buckets[idx].0 += r.prompt_tokens;
                    buckets[idx].1 += r.completion_tokens;
                    buckets[idx].2 += super::pricing::calculate_cost(
                        &r.model,
                        r.prompt_tokens,
                        r.completion_tokens,
                        r.cached_tokens,
                        r.cache_write_tokens,
                        r.ts,
                    );
                }
            }
            Ok(buckets
                .into_iter()
                .enumerate()
                .map(|(i, (p, c, cost))| {
                    let t = now - (24 - 1 - i) as u64 * 60 * 60 * 1000;
                    ChartPoint {
                        label: hour_label_of(t),
                        prompt_tokens: p,
                        completion_tokens: c,
                        cost,
                    }
                })
                .collect())
        }
    }
}

/// 查询明细行（ts 区间，旧在前），供实时聚合用。
///
/// 不读取存储的 `cost`：实时统计按明细的 token 与时刻用当前价目重算，
/// 使分档计费与闲/忙时费率能正确生效。
struct Row {
    ts: u64,
    model: String,
    endpoint: String,
    prompt_tokens: u64,
    completion_tokens: u64,
    cached_tokens: u64,
    cache_write_tokens: u64,
}

/// 按时间区间读取用量明细（`[from_ts, to_ts]` 闭区间，按时间升序），供趋势图与汇总实时聚合。
fn query_rows(conn: &Connection, from_ts: u64, to_ts: u64) -> Result<Vec<Row>, String> {
    let mut stmt = conn
        .prepare(
            "SELECT ts, model, endpoint, prompt_tokens, completion_tokens, cached_tokens, cache_write_tokens
             FROM usage_history WHERE ts >= ?1 AND ts <= ?2 ORDER BY ts ASC",
        )
        .map_err(|e| {
            let e = e.to_string();
            i18n::err_args("query_rows_failed", &[&e])
        })?;
    let rows = stmt
        .query_map(params![from_ts as i64, to_ts as i64], |r| {
            Ok(Row {
                ts: r.get::<_, i64>(0)? as u64,
                model: r.get(1)?,
                endpoint: r.get(2)?,
                prompt_tokens: r.get::<_, i64>(3)? as u64,
                completion_tokens: r.get::<_, i64>(4)? as u64,
                cached_tokens: r.get::<_, i64>(5)? as u64,
                cache_write_tokens: r.get::<_, i64>(6)? as u64,
            })
        })
        .map_err(|e| {
            let e = e.to_string();
            i18n::err_args("iterate_rows_failed", &[&e])
        })?;
    let mut out = Vec::new();
    for r in rows {
        out.push(r.map_err(|e| {
            let e = e.to_string();
            i18n::err_args("read_rows_failed", &[&e])
        })?);
    }
    Ok(out)
}

/// 最近 `limit` 条请求明细（新在前）。
pub fn query_recent(conn: &Connection, limit: usize) -> Result<Vec<RecentRow>, String> {
    let mut stmt = conn
        .prepare(
            "SELECT ts, model, endpoint, status, prompt_tokens, completion_tokens, cached_tokens, cache_write_tokens, cost
             FROM usage_history ORDER BY ts DESC LIMIT ?1",
        )
        .map_err(|e| {
            let e = e.to_string();
            i18n::err_args("query_recent_failed", &[&e])
        })?;
    let rows = stmt
        .query_map(params![limit as i64], |r| {
            Ok(RecentRow {
                ts: r.get::<_, i64>(0)? as u64,
                model: r.get(1)?,
                endpoint: r.get(2)?,
                status: r.get(3)?,
                prompt_tokens: r.get::<_, i64>(4)? as u64,
                completion_tokens: r.get::<_, i64>(5)? as u64,
                cached_tokens: r.get::<_, i64>(6)? as u64,
                cache_write_tokens: r.get::<_, i64>(7)? as u64,
                cost: r.get(8)?,
                elapsed_ms: 0,
            })
        })
        .map_err(|e| {
            let e = e.to_string();
            i18n::err_args("iterate_recent_failed", &[&e])
        })?;
    let mut out = Vec::new();
    for r in rows {
        out.push(r.map_err(|e| {
            let e = e.to_string();
            i18n::err_args("read_recent_failed", &[&e])
        })?);
    }
    Ok(out)
}

/// 清空全部统计（保留表结构），返回清除的记录条数。
pub fn clear_all(conn: &Connection) -> Result<u64, String> {
    let count: i64 = conn
        .query_row("SELECT COUNT(*) FROM usage_history", [], |r| r.get(0))
        .map_err(|e| {
            let e = e.to_string();
            i18n::err_args("count_usage_failed", &[&e])
        })?;
    conn.execute_batch("DELETE FROM usage_history; DELETE FROM usage_daily; DELETE FROM usage_meta;")
        .map_err(|e| {
            let e = e.to_string();
            i18n::err_args("clear_usage_failed", &[&e])
        })?;
    Ok(count as u64)
}

/// 按保留天数清理更早的明细与按天聚合（0 表示永久保留，不做清理）。
/// 返回被清理的明细条数。
pub fn clear_before(conn: &Connection, retention_days: u32) -> Result<u64, String> {
    if retention_days == 0 {
        return Ok(0);
    }
    let cutoff = chrono::Local::now() - chrono::Duration::days(retention_days as i64);
    let cutoff_ts = cutoff.timestamp_millis() as u64;
    let cutoff_key = format!("{:04}-{:02}-{:02}", cutoff.year(), cutoff.month(), cutoff.day());
    let count: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM usage_history WHERE ts < ?1",
            params![cutoff_ts as i64],
            |r| r.get(0),
        )
        .map_err(|e| {
            let e = e.to_string();
            i18n::err_args("count_usage_failed", &[&e])
        })?;
    conn.execute(
        "DELETE FROM usage_history WHERE ts < ?1",
        params![cutoff_ts as i64],
    )
    .map_err(|e| {
        let e = e.to_string();
        i18n::err_args("clear_usage_failed", &[&e])
    })?;
    conn.execute(
        "DELETE FROM usage_daily WHERE date_key < ?1",
        params![cutoff_key],
    )
    .map_err(|e| {
        let e = e.to_string();
        i18n::err_args("clear_usage_failed", &[&e])
    })?;
    Ok(count as u64)
}

/// 把统计数据序列化为前端约定的 JSON（供 stats_get 命令返回）。
pub fn stats_to_json(stats: &UsageStats) -> Value {
    json!({
        "total_requests": stats.total_requests,
        "total_prompt_tokens": stats.total_prompt_tokens,
        "total_completion_tokens": stats.total_completion_tokens,
        "total_cached_tokens": stats.total_cached_tokens,
        "total_cost": stats.total_cost,
        "by_model": stats.by_model,
        "by_endpoint": stats.by_endpoint,
        "last_10_minutes": stats.last_10_minutes,
        "recent_requests": stats.recent_requests,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 建一个临时库连接（内存库便于单测）。
    fn temp_conn() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        conn.pragma_update(None, "journal_mode", "WAL").ok();
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS usage_history (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                ts INTEGER NOT NULL,
                model TEXT NOT NULL,
                endpoint TEXT NOT NULL,
                status TEXT NOT NULL,
                prompt_tokens INTEGER NOT NULL DEFAULT 0,
                completion_tokens INTEGER NOT NULL DEFAULT 0,
                cached_tokens INTEGER NOT NULL DEFAULT 0,
                cache_write_tokens INTEGER NOT NULL DEFAULT 0,
                cost REAL NOT NULL DEFAULT 0,
                stream INTEGER NOT NULL DEFAULT 0
            );
            CREATE INDEX IF NOT EXISTS idx_uh_ts ON usage_history(ts);
            CREATE TABLE IF NOT EXISTS usage_daily (
                date_key TEXT PRIMARY KEY,
                data TEXT NOT NULL
            );
            CREATE TABLE IF NOT EXISTS usage_meta (
                key TEXT PRIMARY KEY,
                value TEXT NOT NULL
            );",
        )
        .unwrap();
        conn
    }

    /// 快捷构造一条用量明细（cache_write 固定 0、流式）。
    fn entry(ts: u64, model: &str, endpoint: &str, status: &str, prompt: u64, completion: u64, cached: u64) -> UsageEntry {
        UsageEntry {
            ts,
            model: model.into(),
            endpoint: endpoint.into(),
            status: status.into(),
            prompt_tokens: prompt,
            completion_tokens: completion,
            cached_tokens: cached,
            cache_write_tokens: 0,
            stream: true,
        }
    }

    /// 记录两条用量后 Today 汇总的请求数、各 token 列与分组正确。
    #[test]
    fn record_and_stats_today() {
        let conn = temp_conn();
        let now = super::super::state::now_millis();
        record_usage(&conn, &entry(now, "deepseek/deepseek-v4-flash", "/v1/chat/completions", "ok", 1000, 500, 200)).unwrap();
        record_usage(&conn, &entry(now - 1000, "claude-sonnet-4-6", "/v1/messages", "ok", 2000, 800, 0)).unwrap();

        let stats = get_stats(&conn, Period::Today).unwrap();
        assert_eq!(stats.total_requests, 2);
        assert_eq!(stats.total_prompt_tokens, 3000);
        assert_eq!(stats.total_completion_tokens, 1300);
        assert_eq!(stats.total_cached_tokens, 200);
        assert_eq!(stats.by_model.len(), 2);
        assert_eq!(stats.by_endpoint.len(), 2);
        assert!(stats.total_cost > 0.0);
    }

    /// 跨天记录在 7D/30D 周期内均被统计。
    #[test]
    fn record_daily_aggregation() {
        let conn = temp_conn();
        let now = super::super::state::now_millis();
        // 两条记录跨天：一条今天、一条 3 天前
        let three_days_ago = now - 3 * 24 * 60 * 60 * 1000;
        record_usage(&conn, &entry(now, "m", "/v1/chat/completions", "ok", 100, 50, 0)).unwrap();
        record_usage(&conn, &entry(three_days_ago, "m", "/v1/chat/completions", "ok", 100, 50, 0)).unwrap();

        // 7D 应统计两条
        let s7 = get_stats(&conn, Period::D7).unwrap();
        assert_eq!(s7.total_requests, 2);
        // 30D 也是两条
        let s30 = get_stats(&conn, Period::D30).unwrap();
        assert_eq!(s30.total_requests, 2);
    }

    /// 趋势图桶数量与周期对应（24H→24 点、7D→7 点）。
    #[test]
    fn chart_buckets() {
        let conn = temp_conn();
        let now = super::super::state::now_millis();
        record_usage(&conn, &entry(now, "m", "/v1/chat/completions", "ok", 100, 50, 0)).unwrap();
        let chart = get_chart(&conn, Period::H24).unwrap();
        assert_eq!(chart.len(), 24);
        // 今天近 7 天应为 7 个点
        let chart7 = get_chart(&conn, Period::D7).unwrap();
        assert_eq!(chart7.len(), 7);
    }

    /// 清空统计返回清除条数，之后汇总归零。
    #[test]
    fn clear_all_works() {
        let conn = temp_conn();
        let now = super::super::state::now_millis();
        record_usage(&conn, &entry(now, "m", "/v1/chat/completions", "ok", 100, 50, 0)).unwrap();
        assert_eq!(clear_all(&conn).unwrap(), 1);
        let stats = get_stats(&conn, Period::All).unwrap();
        assert_eq!(stats.total_requests, 0);
    }

    /// 汇总 JSON 包含前端所需全部字段且分组行数值正确。
    #[test]
    fn stats_json_shape() {
        // 验证 stats_to_json 输出包含前端所需全部字段
        let conn = temp_conn();
        let now = super::super::state::now_millis();
        record_usage(&conn, &entry(now, "deepseek/deepseek-v4-flash", "/v1/chat/completions", "ok", 1000, 500, 200)).unwrap();
        let stats = get_stats(&conn, Period::Today).unwrap();
        let v = stats_to_json(&stats);
        for key in [
            "total_requests",
            "total_prompt_tokens",
            "total_completion_tokens",
            "total_cached_tokens",
            "total_cost",
            "by_model",
            "by_endpoint",
            "last_10_minutes",
            "recent_requests",
        ] {
            assert!(v.get(key).is_some(), "缺少字段 {key}");
        }
        assert_eq!(v["total_requests"].as_u64(), Some(1));
        let rows = v["by_model"].as_array().unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0]["key"], "deepseek/deepseek-v4-flash");
        assert_eq!(rows[0]["total_tokens"].as_u64(), Some(1500));
    }

    /// 趋势图 JSON 的点结构（label 与各 token/cost 列）完整。
    #[test]
    fn chart_json_shape() {
        let conn = temp_conn();
        let now = super::super::state::now_millis();
        record_usage(&conn, &entry(now, "m", "/v1/chat/completions", "ok", 100, 50, 0)).unwrap();
        let chart = get_chart(&conn, Period::D7).unwrap();
        let v = serde_json::to_value(&chart).unwrap();
        let arr = v.as_array().unwrap();
        assert_eq!(arr.len(), 7);
        for p in arr {
            assert!(p.get("label").is_some());
            assert!(p.get("prompt_tokens").is_some());
            assert!(p.get("completion_tokens").is_some());
            assert!(p.get("cost").is_some());
        }
    }

    /// 最近请求明细按时间倒序返回且 token 数正确。
    #[test]
    fn recent_requests() {
        let conn = temp_conn();
        let now = super::super::state::now_millis();
        record_usage(&conn, &entry(now, "m", "/v1/chat/completions", "ok", 100, 50, 0)).unwrap();
        let recent = query_recent(&conn, 10).unwrap();
        assert_eq!(recent.len(), 1);
        assert_eq!(recent[0].model, "m");
        assert_eq!(recent[0].prompt_tokens, 100);
    }

    /// 天键为本地时间的 YYYY-MM-DD 格式。
    #[test]
    fn day_key_local() {
        // 本地时间转天键格式正确
        let now = super::super::state::now_millis();
        let key = day_key_of(now);
        let parts: Vec<&str> = key.split('-').collect();
        assert_eq!(parts.len(), 3);
        assert_eq!(parts[0].len(), 4);
        assert_eq!(parts[1].len(), 2);
        assert_eq!(parts[2].len(), 2);
    }
}
