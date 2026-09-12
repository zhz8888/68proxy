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
    /// 套餐 ID 与展示名；无订阅时为 null / 「无订阅」。
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
    /// 拉取失败原因（成功为 null）。
    pub error: Option<String>,
}

impl AccountQuota {
    /// 构造一个失败占位的额度快照。
    fn failed(user_name: String, masked_key: String, error: String) -> Self {
        Self {
            user_name,
            masked_key,
            plan_id: None,
            plan_name: "无订阅".into(),
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
            return AccountQuota::failed(user_name.into(), masked_key.into(), "whoami 请求失败".into())
        }
    };
    let org_id = whoami
        .pointer("/org/id")
        .and_then(|x| x.as_str())
        .or_else(|| whoami.pointer("/user/id").and_then(|x| x.as_str()))
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
    let period_start = sub_data.and_then(|d| d.get("currentPeriodStart")).and_then(|x| x.as_u64());
    let period_end = sub_data.and_then(|d| d.get("currentPeriodEnd")).and_then(|x| x.as_u64());

    // 额度对象（兼容顶层 credits 与 data.credits 两种包法）
    let credits_obj = credits
        .as_ref()
        .and_then(|v| v.get("credits").or_else(|| v.pointer("/data/credits")));

    // ③ 用量汇总：since 取订阅周期起点（与 CLI 一致）
    let since_q = period_start
        .map(|s| format!("&since={s}"))
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
    let has_billing = credits.is_some() || subs.is_some();
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

    // 窗口限额：credits.windowLimits.{fiveHour,weekly}
    let (five_hour, weekly) = match credits_obj.and_then(|c| c.get("windowLimits")) {
        Some(w) if w.get("limited").and_then(|x| x.as_bool()).unwrap_or(true) => (
            w.get("fiveHour").and_then(LimitWindow::from_json),
            w.get("weekly").and_then(LimitWindow::from_json),
        ),
        _ => (None, None),
    };

    AccountQuota {
        user_name: user_name.to_string(),
        masked_key: masked_key.to_string(),
        plan_name: plan_id.as_deref().map(plan_name).unwrap_or_else(|| "无订阅".into()),
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

    #[test]
    fn limit_window_from_json() {
        let w = LimitWindow::from_json(&json!({ "used": 12, "cap": 50, "resetAt": 1700000000000u64 })).unwrap();
        assert!((w.used - 12.0).abs() < 1e-9);
        assert!((w.cap - 50.0).abs() < 1e-9);
        assert_eq!(w.reset_at, Some(1700000000000));
        // 缺 cap 视为无效窗口
        assert!(LimitWindow::from_json(&json!({ "used": 1 })).is_none());
    }
}
