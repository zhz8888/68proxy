//! 模型单价表与成本估算，数据抓取自 commandcode.ai 的 pricing-limits 文档
//! （单位均为 $/1M tokens）。
//!
//! 计费口径要点：
//! - **分档计费**：按请求输入 token 落在哪一档上下文区间选取费率；
//! - **闲时/忙时**：DeepSeek 系列按 UTC 工作日窗口在 off-peak/peak 两套费率间切换，
//!   `timeOfDay` 中的 `tiers` 即闲时价，命中午夜窗口时改用 `peak`；
//! - **免费模型**：`deal.free` 为真或费率为 0 时不产生成本；
//! - **折扣**：`deal.discountPercent` 为促销标记，抓取到的 `rates` 已是折后有效价，
//!   成本直接按 `rates` 计算、不再二次打折，折扣仅作展示信息。
//!
//! 成本为估算值而非真实账单；未收录的模型回退默认单价。
//!
//! 数据落库与兜底：运行时以 SQLite `model_pricing` 表为准（首次启动由 `pricing.json`
//! 播种、数据更新按 ID 覆盖，见 models 模块）；内嵌文件 `pricing.json` 仅作兜底，
//! 数据库无数据/不可用时才使用。
//!
//! `pricing.json` 由 `tools/fetch-pricing.mjs` 从官方 pricing-limits 文档爬取生成
//! （重跑脚本即可热更新数据文件）；模型基础信息（名称/厂商/上下文/能力）存于
//! `models` 表（数据文件 `models.json`），见 models 模块。

use std::sync::{Arc, OnceLock, RwLock};

use serde::{Deserialize, Serialize};

/// 单档费率（输入 / 输出 / 缓存命中 / 缓存写入，$/1M tokens）。
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize)]
pub struct Rates {
    #[serde(default)]
    pub input: f64,
    #[serde(default)]
    pub output: f64,
    #[serde(default, rename = "cacheRead")]
    pub cached: f64,
    #[serde(default, rename = "cacheWrite")]
    pub cache_write: f64,
}

/// 模型能力标记（文本输入 / 视觉 / 思考）。
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize)]
pub struct ModelCaps {
    #[serde(default)]
    pub text: bool,
    #[serde(default)]
    pub vision: bool,
    #[serde(default)]
    pub reasoning: bool,
}

/// 一个价格档位：`max_context` 为该档覆盖的最大输入 token（None 表示最高档、无上限）。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Tier {
    #[serde(default, rename = "maxContext")]
    pub max_context: Option<u64>,
    pub rates: Rates,
    /// 该档的标牌价（未打折；仅部分促销模型提供），仅供展示。
    #[serde(default, rename = "listRates", skip_serializing_if = "Option::is_none")]
    pub list_rates: Option<Rates>,
}

/// 闲时/忙时费率：`peak` 为忙时价，其余时间用档位价（即闲时价）。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TimeOfDay {
    pub peak: Rates,
    /// 忙时窗口，UTC 小时区间 [start, end)。
    #[serde(default, rename = "peakRanges")]
    pub peak_ranges: Vec<(u8, u8)>,
    /// 忙时窗口是否仅限周一至周五。
    #[serde(default, rename = "weekdaysOnly")]
    pub weekdays_only: bool,
    /// 人类可读窗口描述（仅作展示）。
    #[serde(default)]
    pub windows: String,
}

/// 促销信息：`discount_percent` 为折扣百分比，`free` 表示限时免费。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Deal {
    #[serde(default, rename = "discountPercent")]
    pub discount_percent: u8,
    #[serde(default)]
    pub free: bool,
    /// 促销结束日期（YYYY-MM-DD），存在时仅作展示。
    #[serde(default)]
    pub expires: Option<String>,
    #[serde(default, rename = "endsWhen")]
    pub ends_when: Option<String>,
}

/// 单个模型的价格信息（促销 / 分档费率 / 闲忙时）。
///
/// 模型基础信息（名称 / 厂商 / 上下文长度 / 能力）在列表域，见 `models` 表与
/// `state::ModelInfo`；两者按模型 ID 关联（匹配口径见 `find_pricing`）。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelPricing {
    pub id: String,
    #[serde(default)]
    pub deal: Option<Deal>,
    #[serde(default, rename = "timeOfDay")]
    pub time_of_day: Option<TimeOfDay>,
    #[serde(default)]
    pub tiers: Vec<Tier>,
}

/// 旧版合并表的完整记录：列表信息与价格信息在同一 JSON 中。
///
/// 仅用于旧库迁移时解析旧行（新数据源已拆分为 `models.json` 与 `pricing.json`，
/// 运行时数据分属 `models` 与 `model_pricing` 两张表，见 models 模块）。
#[derive(Debug, Clone, Deserialize)]
pub struct FullModelRecord {
    pub id: String,
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub provider: Option<String>,
    #[serde(default, rename = "contextWindow")]
    pub context_window: Option<u64>,
    #[serde(default)]
    pub caps: ModelCaps,
    #[serde(default)]
    pub deal: Option<Deal>,
    #[serde(default, rename = "timeOfDay")]
    pub time_of_day: Option<TimeOfDay>,
    #[serde(default)]
    pub tiers: Vec<Tier>,
}

/// 未收录模型时的兜底单价。
pub const FALLBACK_PRICE: Rates = Rates {
    input: 1.00,
    output: 3.00,
    cached: 0.10,
    cache_write: 1.00,
};

/// 内嵌的价格数据文件（`tools/fetch-pricing.mjs` 从官方文档爬取生成，随程序打包）。
///
/// 仅作兜底与首次启动播种：运行时读取均以数据库为准。
const PRICING_JSON: &str = include_str!("pricing.json");

/// 运行时模型表：启动时由 SQLite 载入，数据更新时整体替换；`None` 表示尚未载入。
static REGISTRY: RwLock<Option<Arc<Vec<ModelPricing>>>> = RwLock::new(None);

/// 解析内嵌价格文件（进程内只解析一次）；注册表未载入时作兜底。
pub fn builtin_pricing() -> Vec<ModelPricing> {
    static CACHE: OnceLock<Vec<ModelPricing>> = OnceLock::new();
    CACHE.get_or_init(|| {
        serde_json::from_str::<Vec<ModelPricing>>(PRICING_JSON)
            .expect("内置价格文件 pricing.json 解析失败")
    })
    .clone()
}

/// 用给定模型表整体替换运行时注册表（启动时从 SQLite 载入、数据更新落库后刷新）。
pub fn set_models(models: Vec<ModelPricing>) {
    *REGISTRY.write().unwrap() = Some(Arc::new(models));
}

/// 当前生效的模型表：优先注册表（SQLite 载入），未载入时回退内嵌兜底表。
///
/// 返回 `Arc` 便于调用方低成本持有快照，避免在统计重算等高频路径上反复克隆整表。
pub fn all_models() -> Arc<Vec<ModelPricing>> {
    if let Some(models) = REGISTRY.read().unwrap().as_ref() {
        return models.clone();
    }
    static FALLBACK: OnceLock<Arc<Vec<ModelPricing>>> = OnceLock::new();
    FALLBACK
        .get_or_init(|| Arc::new(builtin_pricing()))
        .clone()
}

/// 查找模型计费信息：精确 → 去 provider 前缀的短名 → 最长前缀。
///
/// 上游模型 ID 可能带 provider 前缀（`deepseek/deepseek-v4-flash`）或日期后缀
/// （`claude-haiku-4-5-20251001`），故匹配不区分大小写并取最长前缀。
/// 返回借用，调用方持有 `all_models()` 快照即可复用，避免整条克隆。
pub fn find_pricing<'a>(models: &'a [ModelPricing], model: &str) -> Option<&'a ModelPricing> {
    let target = model.to_ascii_lowercase();

    // 1) 全名精确匹配（忽略大小写）
    if let Some(m) = models.iter().find(|m| m.id.to_ascii_lowercase() == target) {
        return Some(m);
    }
    // 2) 去掉 provider 前缀后的短名精确匹配
    let short = target.rsplit('/').next().unwrap_or(&target);
    if let Some(m) = models.iter().find(|m| m.id.to_ascii_lowercase() == short) {
        return Some(m);
    }
    // 3) 最长前缀匹配，避免短前缀（如 gpt-5）抢走更具体的档位
    models
        .iter()
        .filter(|m| short.starts_with(&m.id.to_ascii_lowercase()))
        .max_by_key(|m| m.id.len())
}

/// 判断给定时刻是否落在忙时窗口内（UTC，窗口定义来自模型自身的 `timeOfDay`）。
///
/// 所有闲忙时模型共用同一窗口（UTC 周一至周五 01–04 与 06–10），故此处用统一规则；
/// 若某模型 `weekdaysOnly` 为假则忽略星期限制。
pub fn is_peak_at(ts_millis: u64, tod: &TimeOfDay) -> bool {
    let secs = (ts_millis / 1000) as i64;
    let Some(dt) = chrono::DateTime::from_timestamp(secs, 0) else {
        return false;
    };
    let dt = dt.naive_utc();
    if tod.weekdays_only {
        use chrono::Datelike;
        // Mon=0 .. Sun=6
        if dt.weekday().num_days_from_monday() >= 5 {
            return false;
        }
    }
    use chrono::Timelike;
    let hour = dt.hour() as u8;
    tod.peak_ranges.iter().any(|(s, e)| hour >= *s && hour < *e)
}

/// 解析指定模型在给定输入规模与时刻下的有效费率。
///
/// 先按输入 token 选档，命中午夜窗口时用忙时价覆盖输入/输出/缓存命中
/// （缓存写入忙时未单独给出，沿用档位价）。
pub fn price_for_at(model: &str, prompt_tokens: u64, ts_millis: u64) -> Rates {
    let models = all_models();
    let Some(m) = find_pricing(&models, model) else {
        return FALLBACK_PRICE;
    };
    let tier_rates = select_tier(m, prompt_tokens);
    match &m.time_of_day {
        Some(tod) if is_peak_at(ts_millis, tod) => Rates {
            cache_write: tier_rates.cache_write,
            ..tod.peak
        },
        _ => tier_rates,
    }
}

/// 选取输入 token 对应的价格档位费率（档位按 max_context 升序，最后一个通常无上限）。
fn select_tier(m: &ModelPricing, prompt_tokens: u64) -> Rates {
    for t in &m.tiers {
        if t.max_context.map(|c| prompt_tokens <= c).unwrap_or(true) {
            return t.rates;
        }
    }
    m.tiers.last().map(|t| t.rates).unwrap_or(FALLBACK_PRICE)
}

/// 把当前生效的模型表序列化为 JSON，供前端展示模型能力/折扣/免费等信息。
pub fn catalog_json() -> serde_json::Value {
    serde_json::to_value(&*all_models()).unwrap_or_else(|_| serde_json::json!([]))
}

/// 根据 token 用量、模型档位与请求时刻估算成本（美元）。
///
/// 输入 token 包含缓存命中与缓存写入两部分（均为输入的子集），计算时先扣除，
/// 避免按全价重复计费；缓存命中按缓存单价、缓存写入按写入单价分别计费。
pub fn calculate_cost(
    model: &str,
    prompt_tokens: u64,
    completion_tokens: u64,
    cached_tokens: u64,
    cache_write_tokens: u64,
    ts_millis: u64,
) -> f64 {
    let p = price_for_at(model, prompt_tokens, ts_millis);
    // cached 与 cache_write 都是 prompt 的子集：两者之和不得超过 prompt，
    // 否则超出部分会被重复计费（1000 命中 + 1000 写入 会被按 2000 输入计价）。
    let cached = cached_tokens.min(prompt_tokens) as f64;
    let cache_write = cache_write_tokens.min(prompt_tokens.saturating_sub(cached as u64)) as f64;
    let non_cached = (prompt_tokens as f64 - cached - cache_write).max(0.0);
    let mut cost = non_cached * p.input / 1e6;
    if cached > 0.0 {
        cost += cached * p.cached / 1e6;
    }
    if cache_write > 0.0 {
        cost += cache_write * p.cache_write / 1e6;
    }
    cost += completion_tokens as f64 * p.output / 1e6;
    cost
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 计费表已加载且覆盖主推模型。
    #[test]
    fn table_loaded_and_covers_common_models() {
        let models = all_models();
        assert!(models.len() >= 60, "计费表条目过少: {}", models.len());
        // 主推模型都应能查到
        for id in [
            "deepseek/deepseek-v4-flash",
            "claude-opus-4-8",
            "claude-sonnet-4-6",
            "gpt-5.5",
            "gpt-5.4",
            "kimi-k3",
        ] {
            let p = find_pricing(&models, id);
            assert!(p.is_some(), "未收录模型 {id}");
        }
    }

    /// 模型匹配忽略 provider 前缀与大小写差异。
    #[test]
    fn provider_prefix_and_case_insensitive() {
        // provider 前缀与大小写不影响匹配
        let models = all_models();
        let a = find_pricing(&models, "deepseek/deepseek-v4-flash").unwrap();
        let b = find_pricing(&models, "deepseek-v4-flash").unwrap();
        assert_eq!(a.id, b.id);
        let c = find_pricing(&models, "moonshotai/Kimi-K2.6").unwrap();
        assert_eq!(c.id, "kimi-k2.6");
    }

    /// 前缀匹配取最长者；带日期后缀的模型名也能命中。
    #[test]
    fn longest_prefix_wins() {
        // gpt-5.6-luna 不应被错误前缀抢走；带日期后缀也能命中
        let p = price_for_at("gpt-5.6-luna-20260101", 1000, 0);
        assert!((p.input - 0.2).abs() < 1e-9);
        let p2 = price_for_at("claude-haiku-4-5-20251001", 1000, 0);
        assert!((p2.input - 1.0).abs() < 1e-9);
    }

    /// 分档计费：按上下文长度落入对应档位。
    #[test]
    fn tier_selection_by_context() {
        // qwen-3.7-flash：≤32K 为 0.03，≤256K 为 0.1，>256K 为 0.2
        assert!((price_for_at("qwen-3.7-flash", 10_000, 0).input - 0.03).abs() < 1e-9);
        assert!((price_for_at("qwen-3.7-flash", 100_000, 0).input - 0.1).abs() < 1e-9);
        assert!((price_for_at("qwen-3.7-flash", 500_000, 0).input - 0.2).abs() < 1e-9);
        // grok-4.6：≤200K 与 >200K 两档
        assert!((price_for_at("grok-4.6", 100_000, 0).input - 2.0).abs() < 1e-9);
        assert!((price_for_at("grok-4.6", 300_000, 0).input - 4.0).abs() < 1e-9);
    }

    /// 闲/忙时费率：工作日按时段切换，周末全天闲时。
    #[test]
    fn off_peak_and_peak_pricing() {
        let models = all_models();
        let m = find_pricing(&models, "deepseek-v4-pro").unwrap();
        let tod = m.time_of_day.as_ref().unwrap();
        // 2026-09-14 是周一：02:00 UTC 为忙时，12:00 UTC 为闲时
        let mon_peak = chrono::DateTime::parse_from_rfc3339("2026-09-14T02:00:00Z")
            .unwrap()
            .timestamp_millis() as u64;
        let mon_off = chrono::DateTime::parse_from_rfc3339("2026-09-14T12:00:00Z")
            .unwrap()
            .timestamp_millis() as u64;
        let sat = chrono::DateTime::parse_from_rfc3339("2026-09-19T02:00:00Z")
            .unwrap()
            .timestamp_millis() as u64;
        assert!(is_peak_at(mon_peak, tod));
        assert!(!is_peak_at(mon_off, tod));
        // 周六同一时刻不算忙时
        assert!(!is_peak_at(sat, tod));
        assert!((price_for_at("deepseek-v4-pro", 1000, mon_peak).input - 1.32).abs() < 1e-9);
        assert!((price_for_at("deepseek-v4-pro", 1000, mon_off).input - 0.66).abs() < 1e-9);
    }

    /// 免费模型任意用量成本为 0。
    #[test]
    fn free_models_cost_zero() {
        // 免费模型（deal.free 且费率为 0）计费应为 0
        for id in ["laguna-s-2.1-free", "ling-3.0-flash-sante:free"] {
            let c = calculate_cost(id, 1_000_000, 1_000_000, 0, 0, 0);
            assert_eq!(c, 0.0, "{id} 应为免费");
            let models = all_models();
            assert!(find_pricing(&models, id).unwrap().deal.as_ref().unwrap().free);
        }
    }

    /// 成本公式：输入/输出/缓存命中分别计价，缓存命中不重复计输入。
    #[test]
    fn cost_basic_and_cache_dedup() {
        // 取无闲忙时模型的固定档位验证成本公式
        let c = calculate_cost("claude-opus-4-8", 1_000_000, 0, 0, 0, 0);
        assert!((c - 5.0).abs() < 1e-9);
        let c = calculate_cost("claude-opus-4-8", 0, 1_000_000, 0, 0, 0);
        assert!((c - 25.0).abs() < 1e-9);
        // 输入全命中缓存：按缓存单价计费，不重复计输入
        let c = calculate_cost("claude-opus-4-8", 1_000_000, 0, 1_000_000, 0, 0);
        assert!((c - 0.5).abs() < 1e-9);
    }

    /// 未收录模型按兜底单价计费。
    #[test]
    fn unknown_model_falls_back() {
        let c = calculate_cost("unknown/model-x", 1_000_000, 1_000_000, 0, 0, 0);
        assert!((c - 4.0).abs() < 1e-9);
    }

    /// 运行时注册表生效后 all_models 返回注册内容；catalog_json 输出完整结构。
    #[test]
    fn registry_roundtrip_and_catalog() {
        let builtin = builtin_pricing();
        let n = builtin.len();
        set_models(builtin_pricing());
        assert_eq!(all_models().len(), n);
        let catalog = catalog_json();
        assert!(catalog.as_array().unwrap().len() >= n);
        assert!(catalog[0].get("id").is_some());
    }

    /// 缓存写入 token 按写入单价单独计费，不与缓存命中重复。
    #[test]
    fn cache_write_tokens_priced_separately() {
        let models = all_models();
        let rate = find_pricing(&models, "claude-opus-4-8").unwrap().tiers[0].rates;
        // 全量输入命中缓存写入：成本即 cache_write 单价
        let c = calculate_cost("claude-opus-4-8", 1_000_000, 0, 0, 1_000_000, 0);
        assert!((c - rate.cache_write).abs() < 1e-9, "全量缓存写入应按写入单价计价: {c} vs {}", rate.cache_write);
    }

    /// 档位表为空的模型回退兜底单价而非 panic。
    #[test]
    fn empty_tiers_model_uses_fallback_price() {
        let mut models = builtin_pricing();
        models.push(ModelPricing {
            id: "test-empty-tiers".into(),
            deal: None,
            time_of_day: None,
            tiers: vec![],
        });
        set_models(models);
        let c = calculate_cost("test-empty-tiers", 1_000_000, 1_000_000, 0, 0, 0);
        // 空档位 → FALLBACK_PRICE（input 2.0 / output 2.0，与 unknown 模型兜底一致）
        assert!((c - 4.0).abs() < 1e-9, "空档位应走兜底价: {c}");
        set_models(builtin_pricing());
    }
}
