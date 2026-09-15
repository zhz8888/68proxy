//! 套餐可用性：复刻 Command Code CLI 的「当前套餐 → 可用模型」判定。
//!
//! CLI 的 `/model` 选择器并不向服务端拉模型清单（模型注册表编译在 CLI 内），
//! 而是并发拉取 `whoami` / `billing/subscriptions` / `billing/credits` 拿到当前
//! 套餐与按量额度，再用本地规则把不可用模型从选择器里过滤掉。本模块复刻同一套
//! 规则，供前端在模型页标注可用性——**仅作提示**，真正的准入由上游在生成时校验。
//!
//! 判定顺序（与 CLI `evaluateModelAccess` 一致）：
//! 1. 存在按量额度（购买或赠送）→ 放行；
//! 2. 无有效套餐 / 套餐未知 / 模型未收录分类 → 放行（未知即不限制）；
//! 3. 否则要求模型分类在套餐允许集合内，且不在套餐屏蔽名单内。

use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};

use serde::Serialize;
use serde_json::Value;

use super::log;
use super::state::{now_millis, AppState};
use crate::i18n;

/// 模型分类：premium（高级）与 opensource（开源）。
const CAT_PREMIUM: &str = "premium";
const CAT_OSS: &str = "opensource";

/// 套餐数据缓存有效期（5 分钟），避免每次进入模型页都请求上游。
const CACHE_TTL_MS: u64 = 5 * 60 * 1000;

/// 套餐数据缓存（进程级：内容为同一账户的套餐信息，无需按实例区分）。
static CACHE: Mutex<Option<(PlanContext, u64)>> = Mutex::new(None);

/// 当前套餐上下文（对应 CLI `createBilling().get()` 的结果）。
#[derive(Debug, Clone, Serialize)]
pub struct PlanContext {
    /// 套餐 ID（如 individual-go）；无有效订阅时为 null。
    pub plan_id: Option<String>,
    /// 套餐展示名（如 Go / GOAT / Pro / Max）；无套餐时为空字符串（前端按当前语言显示「无订阅」）。
    pub plan_name: String,
    /// 已购买按量额度（美元，可能为小数）。
    pub purchased_credits: f64,
    /// 赠送额度（美元，可能为小数）。
    pub free_credits: f64,
    /// 是否拉取失败（失败时不做任何限制）。
    pub fetch_failed: bool,
    /// 附加说明：不展示给用户的内部原因码（如 `plan_fetch_failed` / `no_account`），空串表示无。
    pub note: String,
}

impl PlanContext {
    /// 拉取失败或不可用时的放行上下文。`note` 为原因码（见模块说明），前端按需映射。
    fn unavailable(reason: &str) -> Self {
        Self {
            plan_id: None,
            // 空串作为「无订阅」哨兵：文案由前端按当前语言渲染
            plan_name: String::new(),
            purchased_credits: 0.0,
            free_credits: 0.0,
            fetch_failed: true,
            note: reason.into(),
        }
    }
}

/// 单个模型的准入判定结果。
#[derive(Debug, Clone, Serialize)]
pub struct AccessInfo {
    /// 当前套餐下是否可用。
    pub allowed: bool,
    /// 不可用时给出最低需要的套餐名（如 GOAT / Provider）。
    pub minimum_plan: Option<String>,
    /// 不可用原因：留空，由前端按当前语言结合 `minimum_plan` 组装提示文案。
    pub reason: Option<String>,
}

impl AccessInfo {
    /// 构造「模型可用」结果（无需套餐、无原因）。
    fn allowed() -> Self {
        Self { allowed: true, minimum_plan: None, reason: None }
    }
}

/// 套餐准入规则：允许的模型分类 + 额外屏蔽的模型（已归一化）。
struct PlanRule {
    categories: &'static [&'static str],
    blocked: &'static [&'static str],
}

/// 套餐规则表（复刻 CLI 常量 `jr`；屏蔽名单已按归一化模型名展开）。
fn plan_rule(plan_id: &str) -> Option<PlanRule> {
    // Go：仅开源模型，且额外屏蔽若干高质量开源模型
    const GO_BLOCKED: &[&str] = &[
        "muse-spark-1.2",
        "muse-spark-1.3",
        "grok-4.6",
        "gemini-3.7-flash",
        "gemini-3.8-flash",
        "gpt-5.6-sol",
    ];
    // Pro / Pro v1：开放在 premium，但屏蔽 Anthropic 顶配与部分前沿模型
    const PRO_BLOCKED: &[&str] = &[
        "claude-fable-5-1",
        "claude-fable-5",
        "claude-opus-5",
        "claude-opus-4-8",
        "claude-opus-4-7",
        "claude-opus-4-6",
        "claude-opus-4-5",
        "gpt-6-astra",
        "fugu-ultra",
    ];
    const ALL: &[&str] = &[CAT_PREMIUM, CAT_OSS];
    const OSS: &[&str] = &[CAT_OSS];
    Some(match plan_id {
        "individual-go" => PlanRule { categories: OSS, blocked: GO_BLOCKED },
        "individual-goat" => PlanRule { categories: OSS, blocked: &[] },
        "individual-pro" | "individual-pro-v1" => PlanRule { categories: ALL, blocked: PRO_BLOCKED },
        "individual-provider" | "individual-max" | "individual-ultra" | "teams-pro" => {
            PlanRule { categories: ALL, blocked: &[] }
        }
        _ => return None,
    })
}

/// 套餐顺序（由低到高），用于计算某模型的最低可用套餐。
const PLAN_ORDER: &[&str] = &[
    "individual-go",
    "individual-goat",
    "individual-pro",
    "individual-pro-v1",
    "individual-provider",
    "individual-max",
    "individual-ultra",
    "teams-pro",
];

/// 套餐展示名（复刻 CLI `Wr`；未收录时按 individual-/teams- 规则推导）。
fn plan_display_name(plan_id: &str) -> String {
    match plan_id {
        "individual-go" => "Go".into(),
        "individual-goat" => "GOAT".into(),
        "individual-pro" | "individual-pro-v1" => "Pro".into(),
        "individual-provider" => "Provider".into(),
        "individual-max" => "Max".into(),
        "individual-ultra" => "Ultra".into(),
        "teams-pro" => "Teams Pro".into(),
        other => {
            let s = other.replace("individual-", "").replace("teams-", "Teams ");
            s.split(' ')
                .map(|w| {
                    let mut c = w.chars();
                    match c.next() {
                        Some(f) => f.to_uppercase().collect::<String>() + c.as_str(),
                        None => String::new(),
                    }
                })
                .collect::<Vec<_>>()
                .join(" ")
        }
    }
}

/// 归一化模型 ID：转小写 → 去 `provider:` 前缀 → 去 `vendor/` 前缀 → 去 8 位日期后缀。
///
/// 上游/CLI/定价文档三方的 ID 命名风格不同（`moonshotai/Kimi-K3` vs `kimi-k3`、
/// `claude-haiku-4-5-20251001` vs `claude-haiku-4-5`），归一化后即可互通。
fn normalize(model_id: &str) -> String {
    let s = model_id.trim().to_lowercase();
    let s = s.rsplit(':').next().unwrap_or(&s);
    let s = s.rsplit('/').next().unwrap_or(s);
    // 剥离尾部 8 位日期（-20251001 或 @20251001）
    let bytes = s.as_bytes();
    if bytes.len() > 9 {
        let sep = bytes[bytes.len() - 9];
        if (sep == b'-' || sep == b'@') && bytes[bytes.len() - 8..].iter().all(|b| b.is_ascii_digit())
        {
            return s[..s.len() - 9].to_string();
        }
    }
    s.to_string()
}

/// 去连字符/点的紧凑形式，用于兜住 `Qwen/Qwen3.7-Max` ↔ `qwen-3.7-max` 这类命名差异。
fn compact(norm: &str) -> String {
    norm.chars().filter(|c| *c != '-' && *c != '.').collect()
}

/// 模型分类表（复刻 CLI 常量 `Ur`，键已归一化）。
///
/// 只收录区分套餐所需的关键项：premium 决定 Go/GOAT 是否可用，其余均为 opensource。
/// 未收录的模型按「不限制」处理（与 CLI 一致）。
fn category_table() -> &'static HashMap<&'static str, &'static str> {
    static TABLE: OnceLock<HashMap<&'static str, &'static str>> = OnceLock::new();
    TABLE.get_or_init(|| {
        let mut m = HashMap::new();
        // premium：Anthropic 全系 + OpenAI 直连部分 + 少数网关前沿模型
        for id in [
            "claude-sonnet-5",
            "claude-sonnet-4-6",
            "claude-fable-5-1",
            "claude-fable-5",
            "claude-opus-5",
            "claude-opus-4-8",
            "claude-opus-4-7",
            "claude-opus-4-6",
            "claude-haiku-4-5",
            "gpt-5.6-terra",
            "gpt-6-astra",
            "gpt-5.5",
            "gpt-5.4",
            "gpt-5.4-mini",
            "gpt-5.3-codex",
            "gemini-3.5-flash",
            "gemini-3.1-flash-lite",
            "fugu-ultra",
            "muse-spark-1.1",
        ] {
            m.insert(id, CAT_PREMIUM);
        }
        // opensource：网关/开源池模型（含 gpt-5.6-sol/luna、grok、muse-spark-1.2+）
        for id in [
            "gpt-5.6-sol",
            "gpt-5.6-luna",
            "muse-spark-1.2",
            "muse-spark-1.2-contributor",
            "muse-spark-1.3",
            "muse-spark-1.3-contributor",
            "grok-4.5",
            "grok-4.6",
            "gemini-3.7-flash",
            "gemini-3.8-flash",
            "glm-5.3-flash",
            "hy4-preview",
            "hy3-paid",
            "hy3",
            "minimax-m3-free",
            "minimax-m3",
            "minimax-m2.7",
            "minimax-m2.7-free",
            "minimax-m2.5",
            "deepseek-v4-pro",
            "deepseek-v4-flash",
            "deepseek-v4.1-flash",
            "kimi-k3",
            "kimi-k2.7-code",
            "kimi-k2.7-code-highspeed",
            "kimi-k2.6",
            "kimi-k2.5",
            "glm-5.3",
            "glm-5.2",
            "glm-5.2-fast",
            "glm-5.1",
            "glm-5",
            "mimo-v2.5-pro",
            "mimo-v2.5",
            "qwen3.6-max-preview",
            "qwen3.6-plus",
            "qwen3.7-max",
            "qwen3.7-plus",
            "qwen3.8-max-0902",
            "qwen3.8-max",
            "qwen3.8-27b",
            "qwen3.8-flash",
            "qwen3.7-flash",
            "longcat-2.0:free",
            "ling-3.0-flash-sante:free",
            "step-3.7-flash",
            "step-3.5-flash",
            "nemotron-3-ultra-550b-a55b",
            "inkling",
        ] {
            m.insert(id, CAT_OSS);
        }
        m
    })
}

/// 分类索引：归一化键 → 分类，以及紧凑键 → 分类（兜住 `qwen-3.7-max`↔`qwen3.7-max`）。
fn category_index() -> &'static (HashMap<&'static str, &'static str>, HashMap<String, &'static str>) {
    static INDEX: OnceLock<(HashMap<&'static str, &'static str>, HashMap<String, &'static str>)> =
        OnceLock::new();
    INDEX.get_or_init(|| {
        let table = category_table();
        let compacted = table.iter().map(|(k, v)| (compact(k), *v)).collect();
        (table.clone(), compacted)
    })
}

/// 查询归一化模型的分类：先精确匹配，再尝试紧凑形式（兜住 dash 风格差异）。
fn model_category(norm: &str) -> Option<&'static str> {
    let (exact, compacted) = category_index();
    if let Some(c) = exact.get(norm) {
        return Some(*c);
    }
    compacted.get(compact(norm).as_str()).copied()
}

/// 判断模型是否命中套餐屏蔽名单（归一化或紧凑形式任一相同即算命中）。
fn is_blocked(blocked: &[&str], norm: &str) -> bool {
    let c = compact(norm);
    blocked
        .iter()
        .any(|b| normalize(b) == norm || compact(&normalize(b)) == c)
}

/// 判定某模型在当前套餐下是否可用（不触发网络请求）。
pub fn evaluate_access(model_id: &str, ctx: &PlanContext) -> AccessInfo {
    // 0. 拉取失败：不限制（与 CLI 的 fetchFailed 兜底一致）
    if ctx.fetch_failed {
        return AccessInfo::allowed();
    }
    // 1. 按量额度可解锁套餐外模型
    if ctx.purchased_credits > 0.0 || ctx.free_credits > 0.0 {
        return AccessInfo::allowed();
    }
    // 2. 无有效套餐 / 未知套餐：不限制
    let Some(plan_id) = ctx.plan_id.as_deref() else {
        return AccessInfo::allowed();
    };
    let Some(rule) = plan_rule(plan_id) else {
        return AccessInfo::allowed();
    };
    // 3. 模型未收录分类：不限制
    let norm = normalize(model_id);
    let Some(category) = model_category(&norm) else {
        return AccessInfo::allowed();
    };
    let blocked = is_blocked(rule.blocked, &norm);
    if rule.categories.contains(&category) && !blocked {
        return AccessInfo::allowed();
    }
    let minimum = PLAN_ORDER.iter().find_map(|id| {
        let r = plan_rule(id)?;
        (r.categories.contains(&category) && !is_blocked(r.blocked, &norm))
            .then(|| plan_display_name(id))
    });
    AccessInfo {
        allowed: false,
        minimum_plan: minimum,
        // 原因文案由前端按当前语言结合 minimum_plan 组装（见 ModelsView）
        reason: None,
    }
}

/// 取一个 JSON 端点（GET，Bearer 鉴权，10s 超时），失败返回 None。
pub(crate) async fn get_json(state: &AppState, url: &str, api_key: &str) -> Option<Value> {
    let fut = state
        .client()
        .get(url)
        .header(reqwest::header::AUTHORIZATION, format!("Bearer {api_key}"))
        .header(reqwest::header::CONTENT_TYPE, "application/json")
        .send();
    let res = tokio::time::timeout(std::time::Duration::from_secs(10), fut)
        .await
        .ok()?
        .ok()?;
    if !res.status().is_success() {
        return None;
    }
    res.json::<Value>().await.ok()
}

/// 拉取当前套餐上下文：whoami 取 orgId → 并发拉 subscriptions 与 credits。
///
/// 只有订阅状态属于 active/trialing/past_due 才采信 planId；任何一步失败都返回
/// `fetch_failed` 的放行上下文（与 CLI 的兜底一致）。
pub async fn fetch_plan_context(state: &AppState, api_key: &str) -> PlanContext {
    let base = state.config.read().unwrap().api_base.clone();

    // whoami：取组织 ID（CLI 用 org.id）。个人账户 org 为 null 时保持 None，
    // 不带 orgId 参数；不能用 user.id 兜底（上游对非本组织 orgId 返回 403）。
    let whoami = get_json(state, &format!("{base}/alpha/whoami"), api_key).await;
    let org_id = whoami.as_ref().and_then(|v| {
        v.pointer("/org/id")
            .and_then(|x| x.as_str())
            .map(|s| s.to_string())
    });
    let query = org_id.as_deref().map(|o| format!("?orgId={o}")).unwrap_or_default();

    let subs_url = format!("{base}/alpha/billing/subscriptions{query}");
    let credits_url = format!("{base}/alpha/billing/credits{query}");
    let (subs, credits) = tokio::join!(
        get_json(state, &subs_url, api_key),
        get_json(state, &credits_url, api_key),
    );

    // 订阅：仅在 success 且状态有效时采信 planId
    let plan_id = subs.as_ref().and_then(|v| {
        if v.get("success").and_then(|s| s.as_bool()) == Some(false) {
            return None;
        }
        let status = v.pointer("/data/status").and_then(|s| s.as_str()).unwrap_or("");
        let pid = v.pointer("/data/planId").and_then(|s| s.as_str())?;
        matches!(status, "active" | "trialing" | "past_due").then(|| pid.to_string())
    });

    // 额度：credits.purchasedCredits / credits.freeCredits（兼容包在 data 下的写法）
    let credits_obj = credits.as_ref().and_then(|v| v.get("credits").or_else(|| v.pointer("/data/credits")));
    // 这两个字段是美元金额，上游可能返回小数（如 4.5）；用 as_u64 读会把小数静默当 0，
    // 导致「按量额度解锁」判定失败、把本来可用的高级模型误标为不可用
    let read_credits = |key: &str| -> f64 {
        credits_obj
            .and_then(|c| c.get(key))
            .and_then(|x| x.as_f64())
            .unwrap_or(0.0)
    };
    let purchased = read_credits("purchasedCredits");
    let free = read_credits("freeCredits");

    // 与 CLI 一致：whoami / subscriptions / credits.credits 任一缺失即视为拉取失败（放行）
    if whoami.is_none() || subs.is_none() || credits_obj.is_none() {
        log::warn(i18n::pick(
            "套餐信息拉取失败：whoami / subscriptions / credits 不完整",
            "Failed to fetch plan info: whoami / subscriptions / credits incomplete",
        ));
        return PlanContext::unavailable("plan_fetch_failed");
    }

    let ctx = PlanContext {
        plan_name: plan_id.as_deref().map(plan_display_name).unwrap_or_default(),
        plan_id,
        purchased_credits: purchased,
        free_credits: free,
        fetch_failed: false,
        note: String::new(),
    };
    log::info(&format!(
        "{}: {} ({} {} / {} {})",
        i18n::pick("套餐信息已更新", "Plan info updated"),
        if ctx.plan_name.is_empty() {
            i18n::pick("无订阅", "No subscription")
        } else {
            &ctx.plan_name
        },
        i18n::pick("购买额度", "purchased"),
        ctx.purchased_credits,
        i18n::pick("赠送额度", "free"),
        ctx.free_credits
    ));
    ctx
}

/// 带缓存的套餐上下文获取：`force` 为真时强制刷新，否则 5 分钟内复用。
pub async fn plan_context(state: &AppState, api_key: Option<&str>, force: bool) -> PlanContext {
    if !force {
        if let Some((cached, at)) = CACHE.lock().unwrap().as_ref() {
            if now_millis().saturating_sub(*at) < CACHE_TTL_MS {
                return cached.clone();
            }
        }
    }
    let ctx = match api_key {
        Some(k) => fetch_plan_context(state, k).await,
        None => PlanContext::unavailable("no_account"),
    };
    *CACHE.lock().unwrap() = Some((ctx.clone(), now_millis()));
    ctx
}

/// 构造套餐状态响应：套餐信息 + 当前模型表中每个模型的准入结果（键为模型 ID）。
pub fn plan_status_json(ctx: &PlanContext) -> Value {
    let access: serde_json::Map<String, Value> = super::pricing::all_models()
        .iter()
        .map(|m| (m.id.clone(), serde_json::to_value(evaluate_access(&m.id, ctx)).unwrap_or(Value::Null)))
        .collect();
    serde_json::json!({
        "plan": ctx,
        "access": access,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 快捷构造套餐上下文（订阅名 + 购买/赠送额度）。
    fn ctx(plan: Option<&str>, purchased: f64, free: f64) -> PlanContext {
        PlanContext {
            plan_id: plan.map(|s| s.to_string()),
            plan_name: plan.map(plan_display_name).unwrap_or_else(|| "无订阅".into()),
            purchased_credits: purchased,
            free_credits: free,
            fetch_failed: false,
            note: String::new(),
        }
    }

    /// 模型名归一化：去 provider 前缀、日期后缀与网关限定。
    #[test]
    fn normalize_strips_prefix_and_date() {
        assert_eq!(normalize("moonshotai/Kimi-K3"), "kimi-k3");
        assert_eq!(normalize("claude-haiku-4-5-20251001"), "claude-haiku-4-5");
        assert_eq!(normalize("claude-opus-4-8@20250101"), "claude-opus-4-8");
        assert_eq!(normalize("vercel-ai-gateway:meta/muse-spark-1.2"), "muse-spark-1.2");
        assert_eq!(normalize("Qwen/Qwen3.7-Max"), "qwen3.7-max");
    }

    /// 分类查表覆盖不同命名风格；未收录模型不限制。
    #[test]
    fn category_lookup_covers_both_styles() {
        assert_eq!(model_category("claude-opus-4-8"), Some(CAT_PREMIUM));
        assert_eq!(model_category("gpt-5.6-terra"), Some(CAT_PREMIUM));
        // 命名风格差异：qwen-3.7-max ↔ qwen37max
        assert_eq!(model_category("qwen-3.7-max"), Some(CAT_OSS));
        assert_eq!(model_category("kimi-k3"), Some(CAT_OSS));
        // gpt-5.6-sol 走网关，属开源池
        assert_eq!(model_category("gpt-5.6-sol"), Some(CAT_OSS));
        // 未收录 → 不限制
        assert_eq!(model_category("some-unknown-model"), None);
    }

    /// Go 套餐：仅开源池可用，premium 提示 Provider，屏敝模型提示 GOAT。
    #[test]
    fn go_plan_only_opensource_and_blocklist() {
        let c = ctx(Some("individual-go"), 0.0, 0.0);
        // premium 不可用，提示最低套餐
        let opus = evaluate_access("claude-opus-4-8", &c);
        assert!(!opus.allowed);
        assert_eq!(opus.minimum_plan.as_deref(), Some("Provider"));
        // 普通开源可用
        assert!(evaluate_access("kimi-k3", &c).allowed);
        // Go 额外屏蔽的开源模型不可用，最低套餐为 GOAT
        let sol = evaluate_access("gpt-5.6-sol", &c);
        assert!(!sol.allowed);
        assert_eq!(sol.minimum_plan.as_deref(), Some("GOAT"));
    }

    /// Pro 套餐：放开除顶级（opus/gpt-6）外的模型；Max 无屏蔽。
    #[test]
    fn pro_plan_blocks_top_tier() {
        let c = ctx(Some("individual-pro"), 0.0, 0.0);
        assert!(evaluate_access("claude-sonnet-4-6", &c).allowed);
        assert!(evaluate_access("gpt-5.5", &c).allowed);
        assert!(!evaluate_access("claude-opus-4-8", &c).allowed);
        assert!(!evaluate_access("gpt-6-astra", &c).allowed);
        // max 及以上无屏蔽
        let max = ctx(Some("individual-max"), 0.0, 0.0);
        assert!(evaluate_access("claude-opus-4-8", &max).allowed);
    }

    /// 有额度（购买/赠送）即全放开；无套餐/未知套餐/拉取失败一律放行。
    #[test]
    fn credits_unlock_everything_and_unknown_plan_is_permissive() {
        let paid = ctx(Some("individual-go"), 5.0, 0.0);
        assert!(evaluate_access("claude-opus-4-8", &paid).allowed);
        let free = ctx(Some("individual-go"), 0.0, 10.0);
        assert!(evaluate_access("claude-opus-4-8", &free).allowed);
        // 无套餐 / 未知套餐 / 拉取失败：一律放行
        assert!(evaluate_access("claude-opus-4-8", &ctx(None, 0.0, 0.0)).allowed);
        assert!(evaluate_access("claude-opus-4-8", &ctx(Some("weird-plan"), 0.0, 0.0)).allowed);
        let mut failed = ctx(Some("individual-go"), 0.0, 0.0);
        failed.fetch_failed = true;
        assert!(evaluate_access("claude-opus-4-8", &failed).allowed);
    }

    /// 套餐状态 JSON 的结构：套餐信息 + 各模型准入结论。
    #[test]
    fn plan_status_json_shape() {
        let v = plan_status_json(&ctx(Some("individual-go"), 0.0, 0.0));
        assert_eq!(v["plan"]["plan_name"], "Go");
        assert!(v["access"].is_object());
        // 计费表中的模型都应给出准入结论
        assert!(v["access"].get("claude-opus-4-8").is_some());
        assert_eq!(v["access"]["claude-opus-4-8"]["allowed"], false);
    }

    /// 小数金额的按量额度同样解锁：上游金额为浮点时不能读成 0 而误判不可用。
    #[test]
    fn fractional_credits_unlock_models() {
        let tiny = ctx(Some("individual-go"), 4.5, 0.0);
        assert!(evaluate_access("claude-opus-4-8", &tiny).allowed);
        let tiny_free = ctx(Some("individual-go"), 0.0, 0.25);
        assert!(evaluate_access("claude-opus-4-8", &tiny_free).allowed);
    }
}
