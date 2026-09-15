//! 账户额度：复刻 Command Code CLI `/usage` 命令的取数与投影。
//!
//! 取数链路（与 CLI `fetchUsageData` 一致）：
//!   whoami?limits=1 → org.id
//!     → 并发 billing/subscriptions + billing/credits（带 orgId）
//!     → usage/summary?orgId=&since=<订阅周期起点>
//!
//! 展示口径（与 CLI `projectUsageView` 一致）：
//! - 剩余额度分月额度 / 购买额度 / 赠送额度三类，总池 = 月额度 + 购买 + 赠送；
//! - 已用 = 总池 − 剩余，用量百分比即「余额视角」而非账单视角；
//! - 5 小时 / 周窗口限额取自 `credits.windowLimits`，组织级消费限额取自 `whoami.orgLimits`。
//!
//! 所有请求失败都不抛异常：结果里带 `error` 字段，前端据此降级展示。

use serde::Serialize;
use serde_json::Value;

use super::plans::get_json;
use super::state::{now_millis, AppState};

/// 各套餐月额度（$/月），复刻 CLI 常量表。
fn plan_monthly_credits(plan_id: &str) -> Option<f64> {
    // 关键：先把下划线归一为连字符，再用最长前缀匹配，避免 pro-v1 被 pro 抢走
    let norm = plan_id.to_lowercase().replace('_', "-");
    let table: &[(&str, f64)] = &[
        ("individual-provider", 15.0),
        ("individual-ultra", 300.0),
        ("individual-max", 150.0),
        ("individual-pro-v1", 80.0),
        ("individual-goat", 70.0),
        ("teams-pro", 40.0),
        ("individual-pro", 30.0),
        ("individual-go", 10.0),
    ];
    // 已按长度降序排列，首个前缀命中即最长匹配
    table
        .iter()
        .find(|(k, _)| norm.starts_with(k))
        .map(|(_, v)| *v)
}

/// 套餐展示名（与 plans 模块保持同一套口径）。
fn plan_name(plan_id: &str) -> String {
    match plan_id {
        "individual-go" => "Go".into(),
        "individual-goat" => "GOAT".into(),
        "individual-pro" | "individual-pro-v1" => "Pro".into(),
        "individual-provider" => "Provider".into(),
        "individual-max" => "Max".into(),
        "individual-ultra" => "Ultra".into(),
        "teams-pro" => "Teams Pro".into(),
        other => other.to_string(),
    }
}

/// 单个限额窗口（5 小时 / 周）。
#[derive(Debug, Clone, Serialize)]
pub struct LimitWindow {
    /// 已用占比（0-100，已按上限裁剪）。
    pub used: f64,
    /// 上限（上游为额度值或请求数）。
    pub cap: f64,
    /// 窗口重置时间（Unix 毫秒），缺失为 null。
    pub reset_at: Option<u64>,
}

impl LimitWindow {
    /// 从上游窗口 JSON 解析（cap 必填，used 缺省 0，resetAt 缺省 None）；字段缺失返回 None。
    fn from_json(v: &Value) -> Option<Self> {
        let cap = v.get("cap").and_then(|x| x.as_f64())?;
        let used = v.get("used").and_then(|x| x.as_f64()).unwrap_or(0.0);
        Some(Self {
            used,
            cap,
            reset_at: v.get("resetAt").and_then(|x| x.as_u64()),
        })
    }
}

/// 组织级消费限额行（来自 `whoami.orgLimits`）。
#[derive(Debug, Clone, Serialize)]
pub struct OrgLimit {
    pub label: String,
    /// 已用百分比（0-100）。
    pub pct: f64,
    /// 是否已达上限。
    pub reached: bool,
}

/// 单个账户的额度快照（对应 CLI `projectUsageView` 的结果）。
#[derive(Debug, Clone, Serialize)]
pub struct AccountQuota {
    /// 账户显示名。
    pub user_name: String,
    /// 掩码 Key（避免前端暴露完整凭据）。
    pub masked_key: String,
    /// 套餐 ID 与展示名；无订阅时为 null / 空字符串（文案由前端按语言渲染）。
    pub plan_id: Option<String>,
    pub plan_name: String,
    /// 订阅状态（active / trialing / past_due …）。
    pub status: Option<String>,
    /// 三类剩余额度（美元）。
    pub monthly_remaining: f64,
    pub purchased_remaining: f64,
    pub free_remaining: f64,
    /// 总剩余与总池（美元）。
    pub total_remaining: f64,
    pub total_pool: f64,
    /// 本计费周期内上游统计的实际消耗（美元）。
    pub total_spent: f64,
    /// 用量百分比（余额视角，0-100）。
    pub usage_percent: f64,
    /// 是否有可展示的计费数据。
    pub has_billing: bool,
    /// 距离周期重置的天数（向上取整）。
    pub days_left: Option<i64>,
    /// 周期起止（Unix 毫秒）。
    pub period_start: Option<u64>,
    pub period_end: Option<u64>,
    /// 5 小时窗口限额。
    pub five_hour: Option<LimitWindow>,
    /// 周窗口限额。
    pub weekly: Option<LimitWindow>,
    /// 组织级消费限额。
    pub org_limits: Vec<OrgLimit>,
    /// 拉取失败原因码（成功为 null，码表见前端 `quota.error.*`）。
    pub error: Option<String>,
}

impl AccountQuota {
    /// 构造一个失败占位的额度快照。`error` 为错误码（如 `whoami_failed`），由前端翻译。
    fn failed(user_name: String, masked_key: String, error: String) -> Self {
        Self {
            user_name,
            masked_key,
            plan_id: None,
            // 空串作为「无订阅」哨兵：文案由前端按当前语言渲染
            plan_name: String::new(),
            status: None,
            monthly_remaining: 0.0,
            purchased_remaining: 0.0,
            free_remaining: 0.0,
            total_remaining: 0.0,
            total_pool: 0.0,
            total_spent: 0.0,
            usage_percent: 0.0,
            has_billing: false,
            days_left: None,
            period_start: None,
            period_end: None,
            five_hour: None,
            weekly: None,
            org_limits: Vec::new(),
            error: Some(error),
        }
    }
}

/// 单个限额窗口是否已耗尽（无上限或未提供视为未耗尽）。
pub fn window_exhausted(win: &Option<LimitWindow>) -> bool {
    match win {
        Some(w) if w.cap > 0.0 => w.used >= w.cap,
        _ => false,
    }
}

/// 判定账户额度是否已耗尽，判定顺序为 **5 小时 → 周 → 月**（与需求一致）。
///
/// 任一层级判定耗尽即视为不可用：上层窗口先耗尽意味着此刻已无法继续请求，
/// 无需再看更宽的周期。所有额度信息都拿不到（拉取失败/无计费数据）时返回 false，
/// 即「未知不限制」，避免因上游抖动而误判为耗尽、把请求全挤到个别账户。
pub fn is_exhausted(q: &AccountQuota) -> bool {
    if q.error.is_some() {
        return false;
    }
    // ① 5 小时窗口
    if window_exhausted(&q.five_hour) {
        return true;
    }
    // ② 周窗口
    if window_exhausted(&q.weekly) {
        return true;
    }
    // ③ 月配额（余额视角：总池已扣完，或用量百分比触顶）
    if q.has_billing && q.total_pool > 0.0 && (q.total_remaining <= 0.0 || q.usage_percent >= 100.0) {
        return true;
    }
    false
}

/// 账户「剩余额度」评分，用于在需要换账户时挑选余量最多者（越大越优先）。
///
/// 以最窄的可用窗口为基准（5 小时 → 周 → 月），取其剩余比例；这样余量百分比更贴近
/// 近期可用空间，而不是被更宽周期的大额度摊平。无任何窗口信息时回退月配额余额比例。
pub fn remaining_score(q: &AccountQuota) -> f64 {
    let ratio = |w: &Option<LimitWindow>| -> Option<f64> {
        match w {
            Some(w) if w.cap > 0.0 => Some(((w.cap - w.used) / w.cap).clamp(0.0, 1.0)),
            _ => None,
        }
    };
    if let Some(r) = ratio(&q.five_hour) {
        return r;
    }
    if let Some(r) = ratio(&q.weekly) {
        return r;
    }
    if q.total_pool > 0.0 {
        return (q.total_remaining / q.total_pool).clamp(0.0, 1.0);
    }
    0.0
}

/// 额度缓存有效期：60 秒。额度随用量变化，路由判定用缓存即可，
/// 实际值由后台任务定期刷新，请求路径不做网络请求。
pub const CACHE_TTL_MS: u64 = 60 * 1000;

/// 并发刷新全部账户的额度快照并写入缓存（后台任务与命令共用）。
///
/// - `force` 为真时忽略 TTL 强制拉取；
/// - **单飞**：同一账户已有拉取在途时跳过，避免并发重复请求上游；
/// - 同时清理已删除账户的缓存条目，防止无限增长。
pub async fn refresh_all_caches(state: &std::sync::Arc<AppState>, force: bool) {
    use futures_util::future::join_all;

    let accounts = crate::credentials::accounts_from_state(state);
    let known: std::collections::HashSet<String> =
        accounts.iter().map(|a| a.user_id.clone()).collect();
    let now = now_millis();

    let mut to_fetch = Vec::new();
    {
        let mut cache = state.quota_cache.lock().unwrap();
        let mut inflight = state.quota_inflight.lock().unwrap();
        // 清理已被删除账户的缓存与在途标记，防止无限增长
        cache.retain(|id, _| known.contains(id));
        inflight.retain(|id| known.contains(id));
        for a in &accounts {
            if !force {
                if let Some((_, at)) = cache.get(&a.user_id) {
                    if now.saturating_sub(*at) < CACHE_TTL_MS {
                        continue;
                    }
                }
            }
            if inflight.insert(a.user_id.clone()) {
                to_fetch.push(a.clone());
            }
        }
    }

    let futs = to_fetch.into_iter().map(|a| {
        let st = state.clone();
        async move {
            let name = if a.user_name.is_empty() { a.user_id.clone() } else { a.user_name.clone() };
            let masked = crate::credentials::mask_key(&a.key);
            let quota = fetch_account_quota(&st, &name, &masked, &a.key).await;
            st.quota_cache
                .lock()
                .unwrap()
                .insert(a.user_id.clone(), (quota, now_millis()));
            st.quota_inflight.lock().unwrap().remove(&a.user_id);
        }
    });
    join_all(futs).await;
}

/// 取全部账户的额度快照供前端展示：优先用缓存，未命中时实时拉取并回填。
///
/// 返回顺序与账户列表一致（前端按下标对齐）。
pub async fn snapshot_all(state: &std::sync::Arc<AppState>) -> Vec<AccountQuota> {
    use futures_util::future::join_all;

    let accounts = crate::credentials::accounts_from_state(state);
    let futs = accounts.into_iter().map(|a| {
        let st = state.clone();
        async move {
            let name = if a.user_name.is_empty() { a.user_id.clone() } else { a.user_name.clone() };
            let masked = crate::credentials::mask_key(&a.key);
            // 命中未过期缓存则直接复用，避免每次进页面都打上游
            let cached = st
                .quota_cache
                .lock()
                .unwrap()
                .get(&a.user_id)
                .filter(|(_, at)| now_millis().saturating_sub(*at) < CACHE_TTL_MS)
                .map(|(q, _)| q.clone());
            if let Some(q) = cached {
                return q;
            }
            let q = fetch_account_quota(&st, &name, &masked, &a.key).await;
            st.quota_cache
                .lock()
                .unwrap()
                .insert(a.user_id.clone(), (q.clone(), now_millis()));
            q
        }
    });
    join_all(futs).await
}

/// 判定上游错误是否表示「当前账户额度耗尽」，用于即时失效该账户的路由绑定。
///
/// 上游在额度用尽时返回 402，或在 200/4xx 消息中携带终态标记
/// （`premium_credits_exhausted` / `insufficient credits` / `model_not_in_plan`）。
/// 命中后无需等待下一次额度轮询，立即把该账户标记为耗尽并让会话改路由。
pub fn looks_exhausted_error(status: u16, body: &str) -> bool {
    const MARKERS: &[&str] = &[
        "premium_credits_exhausted",
        "insufficient credits",
        "insufficient_credits",
        "model_not_in_plan",
        "credits exhausted",
    ];
    if status == 402 {
        return true;
    }
    let lower = body.to_ascii_lowercase();
    MARKERS.iter().any(|m| lower.contains(m))
}

/// 把某账户标记为额度耗尽：覆盖缓存的耗尽态并清除其全部会话绑定。
///
/// 缓存里没有该账户时不做处理（无法凭空构造耗尽快照，交由后台轮询纠正）。
/// 仅在 `priority` 策略下有实际影响（绑定用于粘滞）。
pub fn mark_exhausted(state: &AppState, user_id: &str) {
    {
        let mut cache = state.quota_cache.lock().unwrap();
        if let Some((q, _)) = cache.get_mut(user_id) {
            // 把最窄的可用窗口推到上限，使 is_exhausted 判定为真；无可改窗口时置空池
            match q.five_hour.as_mut() {
                Some(w) if w.cap > 0.0 => w.used = w.cap,
                _ => match q.weekly.as_mut() {
                    Some(w) if w.cap > 0.0 => w.used = w.cap,
                    _ => {
                        q.total_remaining = 0.0;
                        q.usage_percent = 100.0;
                    }
                },
            }
        }
    }
    state
        .account_bindings
        .lock()
        .unwrap()
        .retain(|_, b| b.user_id != user_id);
}

/// 从 `whoami` 响应里解析 `orgLimits`（数组，字段名容错）。
fn parse_org_limits(whoami: &Value) -> Vec<OrgLimit> {
    let arr = match whoami.get("orgLimits").and_then(|v| v.as_array()) {
        Some(a) => a,
        None => return Vec::new(),
    };
    arr.iter()
        .filter_map(|v| {
            let label = v
                .get("label")
                .or_else(|| v.get("name"))
                .and_then(|x| x.as_str())?
                .to_string();
            // pct 可能是 0-100 的百分数，也可能是 0-1 的比例
            let raw = v.get("pct").and_then(|x| x.as_f64()).unwrap_or(0.0);
            let pct = if raw <= 1.0 { raw * 100.0 } else { raw };
            let reached = v.get("reached").and_then(|x| x.as_bool()).unwrap_or(pct >= 100.0);
            Some(OrgLimit { label, pct, reached })
        })
        .collect()
}

/// 把上游的 ISO 8601 时间字符串解析为 Unix 毫秒（供天数计算与展示）；无法解析返回 None。
fn iso_to_millis(s: &str) -> Option<u64> {
    chrono::DateTime::parse_from_rfc3339(s)
        .ok()
        .map(|dt| dt.timestamp_millis().max(0) as u64)
}

/// 拉取单个账户的额度快照。
pub async fn fetch_account_quota(
    state: &AppState,
    user_name: &str,
    masked_key: &str,
    api_key: &str,
) -> AccountQuota {
    let base = state.config.read().unwrap().api_base.clone();

    // ① whoami（带 limits=1，与 CLI 一致）→ orgId 与组织限额
    let whoami = get_json(state, &format!("{base}/alpha/whoami?limits=1"), api_key).await;
    let whoami = match whoami {
        Some(v) => v,
        None => {
            return AccountQuota::failed(user_name.into(), masked_key.into(), "whoami_failed".into())
        }
    };
    // 组织 ID 只取 whoami.org.id；个人账户 org 为 null 时保持 None（不带 orgId 参数）。
    // 不能用 user.id 兜底：上游 billing 端点对非本组织的 orgId 返回 403，
    // 个人账户会把 userId 误当 orgId 拼进 URL 导致订阅/额度全部拉取失败。
    let org_id = whoami
        .pointer("/org/id")
        .and_then(|x| x.as_str())
        .map(|s| s.to_string());
    let query = org_id.as_deref().map(|o| format!("?orgId={o}")).unwrap_or_default();

    // ② 并发拉订阅与额度（URL 先绑定，避免临时值跨 await 被回收）
    let subs_url = format!("{base}/alpha/billing/subscriptions{query}");
    let credits_url = format!("{base}/alpha/billing/credits{query}");
    let (subs, credits) = tokio::join!(
        get_json(state, &subs_url, api_key),
        get_json(state, &credits_url, api_key),
    );

    // 订阅：仅在 success 且状态有效时采信 planId 与周期
    let sub_data = subs.as_ref().and_then(|v| {
        if v.get("success").and_then(|s| s.as_bool()) == Some(false) {
            return None;
        }
        v.get("data")
    });
    let status = sub_data
        .and_then(|d| d.get("status"))
        .and_then(|s| s.as_str())
        .map(|s| s.to_string());
    let plan_id = sub_data
        .and_then(|d| d.get("planId"))
        .and_then(|s| s.as_str())
        .filter(|_| matches!(status.as_deref(), Some("active") | Some("trialing") | Some("past_due")))
        .map(|s| s.to_string());
    // 周期起止：上游返回 ISO 8601 字符串（如 2026-09-14T18:52:47.000Z），
    // 解析为 Unix 毫秒供展示与天数计算；since 参数则保留原始 ISO 字符串。
    let period_start_iso = sub_data.and_then(|d| d.get("currentPeriodStart")).and_then(|x| x.as_str()).map(str::to_string);
    let period_end_iso = sub_data.and_then(|d| d.get("currentPeriodEnd")).and_then(|x| x.as_str()).map(str::to_string);
    let period_start = period_start_iso.as_deref().and_then(iso_to_millis);
    let period_end = period_end_iso.as_deref().and_then(iso_to_millis);

    // 额度响应：完整 body（兼容顶层 `{credits, windowLimits}` 与 `data` 包裹两种包法）
    let credits_body = credits.as_ref().and_then(|v| {
        if v.get("success").and_then(|s| s.as_bool()) == Some(false) {
            None
        } else {
            Some(v.get("data").unwrap_or(v))
        }
    });
    // 额度对象：`credits` 顶层字段（或 data.credits）
    let credits_obj = credits_body.and_then(|v| v.get("credits"));
    // 窗口限额与 credits 平级（CLI 读 e.credits?.windowLimits），兼容旧式内层写法
    let window_limits = credits_body
        .and_then(|v| v.get("windowLimits"))
        .or_else(|| credits_obj.and_then(|c| c.get("windowLimits")));

    // 订阅异常（如 Go 账户无 subscription 记录）时，planId 可从 credits 兜底
    // （CLI 的 credits.planId 用于遥测身份，同一字段可作展示兜底）
    let plan_id = plan_id.or_else(|| {
        credits_obj
            .and_then(|c| c.get("planId"))
            .and_then(|s| s.as_str())
            .map(|s| s.to_string())
    });

    // ③ 用量汇总：since 取订阅周期起点（与 CLI 一致，传原始 ISO 字符串，
    // 上游要求 ISO 8601 datetime，传毫秒会返回 400 Validation error）。
    // 前缀按 query 是否为空区分：query 为空时用 ?，否则用 &，
    // 否则 org=null 时 URL 会变成 .../summary&since=...（缺 ?）导致 404。
    let since_q = period_start_iso
        .as_deref()
        .map(|s| format!("{}since={s}", if query.is_empty() { "?" } else { "&" }))
        .unwrap_or_default();
    let summary = get_json(
        state,
        &format!("{base}/alpha/usage/summary{query}{since_q}"),
        api_key,
    )
    .await;

    let read_f = |key: &str| -> f64 {
        credits_obj
            .and_then(|c| c.get(key))
            .and_then(|x| x.as_f64())
            .unwrap_or(0.0)
    };
    let monthly = read_f("monthlyCredits");
    let purchased = read_f("purchasedCredits");
    let free = read_f("freeCredits");
    let total_remaining = monthly + purchased + free;
    let spent = summary
        .as_ref()
        .and_then(|s| {
            // 直接字段优先，兼容包在 data 下的写法
            s.get("totalCost")
                .or_else(|| s.pointer("/data/totalCost"))
        })
        .and_then(|x| x.as_f64())
        .unwrap_or(0.0)
        .max(0.0);
    // 总池：套餐额度与接口回报月额度取较大值，再加购买/赠送
    let base_monthly = plan_id
        .as_deref()
        .and_then(plan_monthly_credits)
        .filter(|_| status.as_deref() == Some("active"))
        .unwrap_or(monthly);
    let total_pool = base_monthly.max(monthly) + purchased + free;
    // 有计费数据：任一账单接口返回过有效响应（CLI 同款口径 Boolean(credits || subscription)）
    let has_billing = credits_body.is_some() || subs.is_some();
    let usage_percent = if total_pool > 0.0 {
        ((total_pool - total_remaining).max(0.0) / total_pool * 100.0).min(100.0)
    } else if spent > 0.0 {
        // 无余额信息时退化为「周期消耗 / 总池」视角
        ((spent) / (spent + total_remaining).max(1.0) * 100.0).min(100.0)
    } else {
        0.0
    };
    let days_left = period_end.map(|end| {
        let end_ms = end as f64;
        let now = now_millis() as f64;
        (((end_ms - now) / 86_400_000.0).ceil()).max(0.0) as i64
    });

    // 窗口限额：windowLimits.{fiveHour,weekly}（与 credits 平级，兼容内层）
    let (five_hour, weekly) = match window_limits {
        Some(w) if w.get("limited").and_then(|x| x.as_bool()).unwrap_or(true) => (
            w.get("fiveHour").and_then(LimitWindow::from_json),
            w.get("weekly").and_then(LimitWindow::from_json),
        ),
        _ => (None, None),
    };

    AccountQuota {
        user_name: user_name.to_string(),
        masked_key: masked_key.to_string(),
        plan_name: plan_id.as_deref().map(plan_name).unwrap_or_default(),
        plan_id,
        status,
        monthly_remaining: monthly,
        purchased_remaining: purchased,
        free_remaining: free,
        total_remaining,
        total_pool,
        total_spent: spent,
        usage_percent,
        has_billing,
        days_left,
        period_start,
        period_end,
        five_hour,
        weekly,
        org_limits: parse_org_limits(&whoami),
        error: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// 套餐月额度按最长前缀匹配，下划线写法等价。
    #[test]
    fn plan_credits_longest_prefix() {
        assert_eq!(plan_monthly_credits("individual-pro-v1"), Some(80.0));
        assert_eq!(plan_monthly_credits("individual-pro"), Some(30.0));
        assert_eq!(plan_monthly_credits("individual-goat"), Some(70.0));
        assert_eq!(plan_monthly_credits("individual-go"), Some(10.0));
        // 下划线写法同样可识别
        assert_eq!(plan_monthly_credits("individual-pro_v1"), Some(80.0));
        assert_eq!(plan_monthly_credits("unknown-plan"), None);
    }

    /// 组织限额解析：0-1 比例与百分比两种刻度都归一为百分比。
    #[test]
    fn org_limits_parse_both_pct_scales() {
        let w = json!({ "orgLimits": [
            { "label": "Monthly", "pct": 42.5, "reached": false },
            { "label": "Daily", "pct": 0.8 }
        ]});
        let rows = parse_org_limits(&w);
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].label, "Monthly");
        assert!((rows[0].pct - 42.5).abs() < 1e-9);
        assert!(!rows[0].reached);
        // 0-1 比例会被换算为百分比，且达到 80% 未标记 reached
        assert!((rows[1].pct - 80.0).abs() < 1e-9);
        assert!(!rows[1].reached);
    }

    /// 窗口解析：字段齐全时成功，缺 cap 视为无效。
    #[test]
    fn limit_window_from_json() {
        let w = LimitWindow::from_json(&json!({ "used": 12, "cap": 50, "resetAt": 1700000000000u64 })).unwrap();
        assert!((w.used - 12.0).abs() < 1e-9);
        assert!((w.cap - 50.0).abs() < 1e-9);
        assert_eq!(w.reset_at, Some(1700000000000));
        // 缺 cap 视为无效窗口
        assert!(LimitWindow::from_json(&json!({ "used": 1 })).is_none());
    }

    /// 构造判定用快照（只需额度字段，其余走默认）。
    fn quota_with(five: Option<(f64, f64)>, weekly: Option<(f64, f64)>, pool: f64, remaining: f64) -> AccountQuota {
        let win = |(u, c): (f64, f64)| LimitWindow { used: u, cap: c, reset_at: None };
        AccountQuota {
            user_name: "u".into(),
            masked_key: "user_…x".into(),
            plan_id: None,
            plan_name: "Go".into(),
            status: None,
            monthly_remaining: remaining,
            purchased_remaining: 0.0,
            free_remaining: 0.0,
            total_remaining: remaining,
            total_pool: pool,
            total_spent: 0.0,
            usage_percent: if pool > 0.0 { (pool - remaining) / pool * 100.0 } else { 0.0 },
            has_billing: true,
            days_left: None,
            period_start: None,
            period_end: None,
            five_hour: five.map(win),
            weekly: weekly.map(win),
            org_limits: Vec::new(),
            error: None,
        }
    }

    /// 耗尽判定：5 小时窗口最先触发。
    #[test]
    fn exhausted_by_five_hour_first() {
        // 5 小时用满即耗尽，即便周/月仍有大量余额
        let q = quota_with(Some((50.0, 50.0)), Some((10.0, 500.0)), 100.0, 90.0);
        assert!(is_exhausted(&q));
    }

    /// 耗尽判定：5 小时未满但周窗口用满。
    #[test]
    fn exhausted_by_weekly_when_five_hour_ok() {
        // 5 小时未满但周用满 → 耗尽
        let q = quota_with(Some((10.0, 50.0)), Some((500.0, 500.0)), 100.0, 90.0);
        assert!(is_exhausted(&q));
    }

    /// 耗尽判定：窗口未满但月池扣完。
    #[test]
    fn exhausted_by_month_when_windows_ok() {
        // 两个窗口都未满，月池扣完 → 耗尽
        let q = quota_with(Some((10.0, 50.0)), Some((10.0, 500.0)), 30.0, 0.0);
        assert!(is_exhausted(&q));
    }

    /// 各项额度都有余量时不判定耗尽。
    #[test]
    fn not_exhausted_when_all_have_room() {
        let q = quota_with(Some((10.0, 50.0)), Some((100.0, 500.0)), 30.0, 20.0);
        assert!(!is_exhausted(&q));
    }

    /// 无额度信息（无计费/拉取失败）不误判为耗尽。
    #[test]
    fn unknown_quota_is_not_exhausted() {
        // 无任何额度信息（拉取失败/无计费）不应误判为耗尽
        let mut q = quota_with(None, None, 0.0, 0.0);
        q.has_billing = false;
        assert!(!is_exhausted(&q));
        let mut failed = quota_with(None, None, 0.0, 0.0);
        failed.error = Some("whoami_failed".into());
        assert!(!is_exhausted(&failed));
    }

    /// 余量评分取最窄可用窗口：5 小时 > 周 > 月池。
    #[test]
    fn remaining_score_uses_narrowest_window() {
        // 以最窄的可用窗口为准：5 小时剩余 80% 优先于月池剩余 90%
        let q = quota_with(Some((10.0, 50.0)), Some((100.0, 500.0)), 100.0, 90.0);
        assert!((remaining_score(&q) - 0.8).abs() < 1e-9);
        // 无 5 小时则退到周；都不存在才用月池
        let q2 = quota_with(None, Some((100.0, 500.0)), 100.0, 90.0);
        assert!((remaining_score(&q2) - 0.8).abs() < 1e-9);
        let q3 = quota_with(None, None, 100.0, 25.0);
        assert!((remaining_score(&q3) - 0.25).abs() < 1e-9);
    }

    /// 上游错误中的耗尽标记识别：402、credits 相关文案命中；普通限流/服务器错误不误判。
    #[test]
    fn detects_exhaustion_markers_in_upstream_error() {
        assert!(looks_exhausted_error(402, ""));
        assert!(looks_exhausted_error(429, "{\"error\":{\"code\":\"premium_credits_exhausted\"}}"));
        assert!(looks_exhausted_error(200, "insufficient credits"));
        assert!(!looks_exhausted_error(500, "internal server error"));
        assert!(!looks_exhausted_error(429, "rate limit exceeded"));
    }
}
