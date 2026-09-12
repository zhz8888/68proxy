//! 凭据管理：Command Code 上游账户 key 列表 + 本地转发鉴权 key，统一存于 SQLite `settings` 表。
//! config.json 不再承载 key。
//!
//! - **Command Code 账户**（`user_` 开头）：可配置多个，请求按轮询切换使用，分散单账户限流/配额压力。
//!   账户列表以 `AppState.config` 为内存真相源（转发热路径零 IO），写库由本模块负责。
//! - **本地转发 key**（`sk_` 开头）：仅本机服务鉴权用，客户端统一用它接入本地代理，
//!   可一键随机生成；该 key 不会发送给 Command Code 上游。

use crate::i18n;
use crate::proxy::config::Account;
use rand::Rng;
use rusqlite::Connection;
use std::sync::atomic::Ordering;
use std::sync::{Mutex, OnceLock};

/// 内存缓存：本地转发 key，避免每次请求都读取数据库。
static LOCAL_CACHE: OnceLock<Mutex<Option<String>>> = OnceLock::new();

/// 获取本地 key 缓存的单例句柄（首次调用时初始化）。
fn local_cache() -> &'static Mutex<Option<String>> {
    LOCAL_CACHE.get_or_init(|| Mutex::new(None))
}

/// 读取内存缓存中的本地转发 key（不读库）；未生成过时返回 None。
pub fn cached_local_key() -> Option<String> {
    local_cache().lock().unwrap().clone()
}

/// 从设置库读取本地转发 key 并刷新内存缓存；空字符串视为未设置（返回 None）。
pub fn load_local_key(conn: &Connection) -> Result<Option<String>, String> {
    let key = super::proxy::settings::load_config(conn).local_api_key.trim().to_string();
    let key = if key.is_empty() { None } else { Some(key) };
    *local_cache().lock().unwrap() = key.clone();
    Ok(key)
}

/// 保存本地转发 key：去除首尾空白后写入设置库，并同步更新内存缓存。
pub fn save_local_key(conn: &Connection, key: &str) -> Result<(), String> {
    let key = key.trim().to_string();
    let mut cfg = super::proxy::settings::load_config(conn);
    cfg.local_api_key = key.clone();
    super::proxy::settings::save_config(conn, &cfg)?;
    *local_cache().lock().unwrap() = Some(key);
    Ok(())
}

/// 删除本地转发 key：清空设置库中的字段并清除内存缓存。
pub fn delete_local_key(conn: &Connection) -> Result<(), String> {
    let mut cfg = super::proxy::settings::load_config(conn);
    cfg.local_api_key = String::new();
    super::proxy::settings::save_config(conn, &cfg)?;
    *local_cache().lock().unwrap() = None;
    Ok(())
}

/// 随机生成一个本地转发 key：`sk-` 前缀 + 32 位随机十六进制字符（安全强度足够，观感同 OpenAI）。
pub fn generate_local_key() -> String {
    let mut rng = rand::thread_rng();
    let hex: String = (0..32).map(|_| format!("{:x}", rng.gen_range(0..16))).collect();
    format!("sk-{hex}")
}

/// 从设置库读取 Command Code 账户列表（去空白、过滤空 key）。
pub fn load_accounts(conn: &Connection) -> Result<Vec<Account>, String> {
    Ok(super::proxy::settings::load_config(conn)
        .cc_accounts
        .into_iter()
        .map(|a| Account {
            key: a.key.trim().to_string(),
            ..a
        })
        .filter(|a| !a.key.is_empty())
        .collect())
}

/// 新增一个 Command Code 账户：以 `user_id` 为唯一标识，同 userId 已存在时更新其 key 与显示名
/// （视为同一账户重新登录/换 key），否则追加。落库后返回更新后的账户列表
/// （调用方负责同步到 AppState.config）。
pub fn add_account(conn: &Connection, acct: &Account) -> Result<Vec<Account>, String> {
    let key = acct.key.trim().to_string();
    if key.is_empty() {
        return Err(i18n::err("account_key_empty"));
    }
    let mut accounts = load_accounts(conn)?;
    if let Some(existing) = accounts.iter_mut().find(|a| !a.user_id.is_empty() && a.user_id == acct.user_id) {
        existing.key = key;
        if !acct.user_name.is_empty() {
            existing.user_name = acct.user_name.clone();
        }
        if acct.source == "oauth" {
            existing.source = "oauth".into();
        }
    } else if !accounts.iter().any(|a| a.key == key) {
        accounts.push(Account {
            key,
            user_id: acct.user_id.clone(),
            user_name: acct.user_name.clone(),
            source: acct.source.clone(),
            added_at: acct.added_at,
        });
    }
    save_accounts(conn, &accounts)?;
    Ok(accounts)
}

/// 移除指定下标的 Command Code 账户（0 起）；下标越界时返回错误。
/// 返回更新后的账户列表（调用方负责同步到 AppState.config）。
pub fn remove_account_at(conn: &Connection, index: usize) -> Result<Vec<Account>, String> {
    let mut accounts = load_accounts(conn)?;
    if index >= accounts.len() {
        let index = index.to_string();
        return Err(i18n::err_args("account_index_out_of_range", &[&index]));
    }
    accounts.remove(index);
    save_accounts(conn, &accounts)?;
    Ok(accounts)
}

/// 更新指定 userId 账户的自定义显示名；未找到时返回错误。
/// 返回更新后的账户列表（调用方负责同步到 AppState.config）。
pub fn rename_account(conn: &Connection, user_id: &str, user_name: &str) -> Result<Vec<Account>, String> {
    let name = user_name.trim().to_string();
    if name.is_empty() {
        return Err(i18n::err("account_name_empty"));
    }
    let mut accounts = load_accounts(conn)?;
    let Some(existing) = accounts.iter_mut().find(|a| a.user_id == user_id) else {
        return Err(i18n::err_args("account_not_found", &[user_id]));
    };
    existing.user_name = name;
    save_accounts(conn, &accounts)?;
    Ok(accounts)
}

/// 把账户列表写入设置库（add/remove 落库用）。
fn save_accounts(conn: &Connection, accounts: &[Account]) -> Result<(), String> {
    let mut cfg = super::proxy::settings::load_config(conn);
    cfg.cc_accounts = accounts.to_vec();
    super::proxy::settings::save_config(conn, &cfg)
}

/// 从 AppState 的配置读取 Command Code 账户列表。
pub fn accounts_from_state(state: &crate::proxy::state::AppState) -> Vec<Account> {
    state.config.read().unwrap().cc_accounts.clone()
}

/// 掩码显示：user_ab12…cd34（长度不足时整体打点）。
pub fn mask_key(key: &str) -> String {
    // 按字符而非字节截断：key 可能含多字节字符（用户手填），字节切片会 panic
    let chars: Vec<char> = key.chars().collect();
    if chars.len() <= 8 {
        "••••".to_string()
    } else {
        let head: String = chars[..5].iter().collect();
        let tail: String = chars[chars.len() - 4..].iter().collect();
        format!("{head}…{tail}")
    }
}

/// 从账户列表轮询取下一个账户：游标递增取模，多账户交替使用。
///
/// 账户列表为空时返回 None，单账户时恒返回该账户。游标挂在 AppState 上，
/// 每个实例（含测试）各自从 0 开始，互不影响。
pub fn next_account(state: &crate::proxy::state::AppState) -> Option<Account> {
    let accounts = accounts_from_state(state);
    if accounts.is_empty() {
        return None;
    }
    let idx =
        state.round_robin.fetch_add(1, Ordering::Relaxed) % accounts.len();
    Some(accounts[idx].clone())
}

/// 会话 → 账户粘滞绑定的有效期：与 CLI 会话时长一致（12 小时）。
///
/// 超期后解除绑定，下次请求重新选账户，避免长期锁死在某个已变化的账户上。
const BINDING_TTL_MS: u64 = 12 * 60 * 60 * 1000;

/// 读取某会话当前绑定的账户（未绑定/超期/账户已被删除时返回 None）。
fn bound_account(state: &crate::proxy::state::AppState, session_key: &str) -> Option<Account> {
    let now = crate::proxy::state::now_millis();
    let bound = {
        let bindings = state.account_bindings.lock().unwrap();
        match bindings.get(session_key) {
            Some(b) if now.saturating_sub(b.bound_at) < BINDING_TTL_MS => b.user_id.clone(),
            _ => return None,
        }
    };
    accounts_from_state(state)
        .into_iter()
        .find(|a| a.user_id == bound)
}

/// 写入/刷新某会话的账户绑定。
fn bind_account(state: &crate::proxy::state::AppState, session_key: &str, user_id: &str) {
    state.account_bindings.lock().unwrap().insert(
        session_key.to_string(),
        crate::proxy::state::AccountBinding {
            user_id: user_id.to_string(),
            bound_at: crate::proxy::state::now_millis(),
        },
    );
}

/// 取该账户的额度快照（路由用，仅读缓存，绝不触发网络请求）。
fn cached_quota(
    state: &crate::proxy::state::AppState,
    user_id: &str,
) -> Option<crate::proxy::quota::AccountQuota> {
    state
        .quota_cache
        .lock()
        .unwrap()
        .get(user_id)
        .map(|(q, _)| q.clone())
}

/// 在候选账户中挑选「剩余额度最多」者；额度未知的账户按 0 分参与比较（仍优于无候选）。
///
/// 返回 (账户, 是否已耗尽)：若所有账户都耗尽，仍返回余量最多者供兜底尝试。
fn pick_max_remaining<'a>(
    state: &crate::proxy::state::AppState,
    accounts: &'a [Account],
) -> Option<(&'a Account, bool)> {
    use crate::proxy::quota;
    let mut best: Option<(&Account, f64, bool)> = None;
    for a in accounts {
        let quota = cached_quota(state, &a.user_id);
        let exhausted = quota.as_ref().map(quota::is_exhausted).unwrap_or(false);
        let score = quota.as_ref().map(quota::remaining_score).unwrap_or(0.0);
        // 优先保留未耗尽者；同为未耗尽/同为耗尽时取余量评分更高者
        let better = match &best {
            None => true,
            Some((_, best_score, best_exhausted)) => match (exhausted, best_exhausted) {
                (false, true) => true,
                (true, false) => false,
                _ => score > *best_score,
            },
        };
        if better {
            best = Some((a, score, exhausted));
        }
    }
    best.map(|(a, _, ex)| (a, ex))
}

/// 按配置策略为一次请求选出账户。
///
/// - `round_robin`：沿用原轮询行为（每次请求换下一个账户）。
/// - `priority`：**优先消耗指定账户 + 会话粘滞**。同一会话固定走同一账户，
///   避免中途换账户导致上游 prompt 缓存失效、额度消耗变快；仅当绑定账户额度
///   耗尽（5 小时 → 周 → 月依次判定）或属首次请求时，才重新选择：
///   优先取配置指定的账户，其耗尽后改为**剩余额度最多**的账户。
///
/// `session_key` 为下游会话标识（x-session-id / prompt_cache_key 等）；为空时无法
/// 维持粘滞，退化为每次按策略重选（无会话连续性的客户端本就不存在缓存损失）。
pub fn route_account(
    state: &crate::proxy::state::AppState,
    session_key: Option<&str>,
) -> Option<Account> {
    let accounts = accounts_from_state(state);
    if accounts.is_empty() {
        return None;
    }
    let cfg = state.config.read().unwrap();
    // 非优先级策略：保持原有轮询语义
    if !cfg.is_priority_strategy() {
        drop(cfg);
        return next_account(state);
    }
    let preferred_id = cfg.preferred_account_id.clone();
    drop(cfg);

    // ① 会话已绑定且绑定账户仍未耗尽：继续使用（保护上游缓存）
    if let Some(key) = session_key {
        if let Some(bound) = bound_account(state, key) {
            let exhausted = cached_quota(state, &bound.user_id)
                .map(|q| crate::proxy::quota::is_exhausted(&q))
                .unwrap_or(false);
            if !exhausted {
                return Some(bound);
            }
            crate::proxy::log::info(crate::i18n::pick(
                "会话绑定账户额度已耗尽，重新选择账户",
                "The session-bound account is exhausted; selecting another account",
            ));
        }
    }

    // ② 重新选择：优先取配置指定的账户（未耗尽时），否则取剩余额度最多者
    let preferred = if preferred_id.is_empty() {
        None
    } else {
        accounts
            .iter()
            .find(|a| a.user_id == preferred_id)
            .filter(|a| {
                !cached_quota(state, &a.user_id)
                    .map(|q| crate::proxy::quota::is_exhausted(&q))
                    .unwrap_or(false)
            })
    };
    let chosen = preferred
        .or_else(|| pick_max_remaining(state, &accounts).map(|(a, _)| a))?;

    // ③ 记录绑定，后续同会话请求复用同一账户
    if let Some(key) = session_key {
        bind_account(state, key, &chosen.user_id);
    }
    Some(chosen.clone())
}

/// 用 API Key 调用上游 `/alpha/whoami` 验证有效性并取回账户身份（userId/userName）。
///
/// 成功返回 `(userId, userName)`；401 表示 key 无效，其他状态/网络错误给出中文描述。
/// `client` 由调用方传入（AppState 的 HTTP 客户端，含出站代理配置），确保验证也走代理。
pub async fn verify_account_key(
    client: &reqwest::Client,
    api_base: &str,
    api_key: &str,
) -> Result<(String, String), String> {
    let url = format!("{api_base}/alpha/whoami");
    let res = tokio::time::timeout(std::time::Duration::from_secs(15), async {
        client
            .get(&url)
            .header("Authorization", format!("Bearer {api_key}"))
            .header("Content-Type", "application/json")
            .send()
            .await
    })
    .await
    .map_err(|_| i18n::err("verify_timeout"))?
    .map_err(|e| {
        let e = e.to_string();
        i18n::err_args("verify_failed", &[&e])
    })?;

    match res.status().as_u16() {
        200 => {
            let v: serde_json::Value = res
                .json()
                .await
                .map_err(|e| {
                    let e = e.to_string();
                    i18n::err_args("whoami_parse_failed", &[&e])
                })?;
            let user_id = v
                .pointer("/user/id")
                .and_then(|x| x.as_str())
                .or_else(|| v.get("id").and_then(|x| x.as_str()))
                .unwrap_or("")
                .to_string();
            let user_name = v
                .pointer("/user/userName")
                .and_then(|x| x.as_str())
                .or_else(|| v.pointer("/user/name").and_then(|x| x.as_str()))
                .unwrap_or("")
                .to_string();
            if user_id.is_empty() {
                return Err(i18n::err("whoami_missing_id"));
            }
            Ok((user_id, user_name))
        }
        401 => Err(i18n::err("api_key_invalid")),
        s => {
            let s = s.to_string();
            Err(i18n::err_args("upstream_verify_failed", &[&s]))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::proxy::config::Config;
    use crate::proxy::settings::init_settings_on;
    use crate::proxy::state::AppState;

    /// 建一个已初始化 settings 表的内存库（测试辅助）。
    fn temp_conn() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        init_settings_on(&conn).unwrap();
        conn
    }

    /// 随机 key 的形态：sk- 前缀 + 32 位 hex，且两次生成不重复。
    #[test]
    fn generate_local_key_format() {
        let k = generate_local_key();
        assert!(k.starts_with("sk-"), "应带 sk- 前缀");
        // sk- + 32 位 hex
        assert_eq!(k.len(), 3 + 32);
        assert!(k[3..].chars().all(|c| c.is_ascii_hexdigit()));
        // 两次生成应不同
        assert_ne!(k, generate_local_key());
    }

    /// 本地 Key 的存取与删除闭环，并同步刷新进程内缓存。
    #[test]
    fn local_key_roundtrip() {
        let conn = temp_conn();
        assert_eq!(load_local_key(&conn).unwrap(), None);
        save_local_key(&conn, "sk-abcdef").unwrap();
        assert_eq!(load_local_key(&conn).unwrap().as_deref(), Some("sk-abcdef"));
        assert_eq!(cached_local_key().as_deref(), Some("sk-abcdef"));
        delete_local_key(&conn).unwrap();
        assert_eq!(load_local_key(&conn).unwrap(), None);
    }

    /// 账户增删改闭环：同 userId 重加更新不新增、按下标删除、越界报错。
    #[test]
    fn accounts_roundtrip() {
        let conn = temp_conn();
        assert!(load_accounts(&conn).unwrap().is_empty());
        add_account(
            &conn,
            &Account {
                key: "user_one".into(),
                user_id: "id_1".into(),
                user_name: "One".into(),
                source: "manual".into(),
                added_at: 1,
            },
        )
        .unwrap();
        add_account(
            &conn,
            &Account {
                key: "user_two".into(),
                user_id: "id_2".into(),
                user_name: "Two".into(),
                source: "oauth".into(),
                added_at: 2,
            },
        )
        .unwrap();
        // 同 userId 重新添加：更新 key 与显示名，不新增条目
        add_account(
            &conn,
            &Account {
                key: "user_one_new".into(),
                user_id: "id_1".into(),
                user_name: "One Renamed".into(),
                source: "oauth".into(),
                added_at: 3,
            },
        )
        .unwrap();
        let list = load_accounts(&conn).unwrap();
        assert_eq!(list.len(), 2);
        assert_eq!(list[0].key, "user_one_new");
        assert_eq!(list[0].user_name, "One Renamed");
        assert_eq!(list[0].source, "oauth");
        assert_eq!(list[1].key, "user_two");
        remove_account_at(&conn, 0).unwrap();
        let list = load_accounts(&conn).unwrap();
        assert_eq!(list.len(), 1);
        assert_eq!(list[0].key, "user_two");
        assert!(remove_account_at(&conn, 5).is_err()); // 越界报错
    }

    /// 按 userId 重命名：去首尾空白、未知 userId 与空白名报错。
    #[test]
    fn rename_account_by_user_id() {
        let conn = temp_conn();
        add_account(
            &conn,
            &Account {
                key: "user_one".into(),
                user_id: "id_1".into(),
                user_name: "One".into(),
                source: "manual".into(),
                added_at: 1,
            },
        )
        .unwrap();
        let list = rename_account(&conn, "id_1", "  我的名字  ").unwrap();
        assert_eq!(list[0].user_name, "我的名字");
        assert!(rename_account(&conn, "missing_id", "x").is_err());
        assert!(rename_account(&conn, "id_1", "   ").is_err()); // 空白名拒绝
    }

    /// 轮询策略：连续选取按 a → b 依次轮转。
    #[test]
    fn round_robin_rotates() {
        // 构造带两个账户的 state：a → b → a → b（按 key 断言）
        let state = AppState::new(Config::default());
        *state.config.write().unwrap() = Config {
            cc_accounts: vec![
                Account {
                    key: "user_a".into(),
                    user_id: "id_a".into(),
                    ..Account::default()
                },
                Account {
                    key: "user_b".into(),
                    user_id: "id_b".into(),
                    ..Account::default()
                },
            ],
            ..Config::default()
        };
        let mut got = Vec::new();
        for _ in 0..4 {
            got.push(next_account(&state).unwrap().key);
        }
        assert_eq!(got, vec!["user_a", "user_b", "user_a", "user_b"]);
    }

    /// 轮询策略：账户列表为空时返回 None。
    #[test]
    fn round_robin_empty() {
        let state = AppState::new(Config::default());
        assert_eq!(next_account(&state), None);
    }

    /// 构造带账户的 state（策略与优先账户可指定）。
    fn state_with(accounts: Vec<Account>, strategy: &str, preferred: &str) -> std::sync::Arc<AppState> {
        let state = AppState::new(Config::default());
        *state.config.write().unwrap() = Config {
            cc_accounts: accounts,
            account_strategy: strategy.into(),
            preferred_account_id: preferred.into(),
            ..Config::default()
        };
        state
    }

    /// 快捷构造测试账户（key/user_id/user_name 均由 id 派生）。
    fn acct(id: &str) -> Account {
        Account { key: format!("user_{id}"), user_id: id.into(), user_name: id.into(), ..Account::default() }
    }

    /// 注入某账户的额度缓存（供路由判定用）。
    fn put_quota(state: &AppState, user_id: &str, five: Option<(f64, f64)>, weekly: Option<(f64, f64)>, pool: f64, remaining: f64) {
        use crate::proxy::quota::{AccountQuota, LimitWindow};
        let win = |(u, c): (f64, f64)| LimitWindow { used: u, cap: c, reset_at: None };
        let q = AccountQuota {
            user_name: user_id.into(),
            masked_key: format!("user_…{user_id}"),
            plan_id: None,
            plan_name: "Go".into(),
            status: Some("active".into()),
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
        };
        state.quota_cache.lock().unwrap().insert(user_id.to_string(), (q, crate::proxy::state::now_millis()));
    }

    /// 优先策略：指定优先账户后，不同会话首次都选它，且同会话持续绑定。
    #[test]
    fn priority_binds_session_to_preferred_account() {
        // 指定 b 为优先账户：不同会话首次都选 b，且同一会话持续绑定 b
        let state = state_with(vec![acct("a"), acct("b")], "priority", "b");
        put_quota(&state, "a", Some((1.0, 50.0)), None, 10.0, 9.0);
        put_quota(&state, "b", Some((1.0, 50.0)), None, 10.0, 9.0);
        let first = route_account(&state, Some("sess-aaaa1111")).unwrap();
        assert_eq!(first.user_id, "b");
        // 第二个会话同样优先 b（不是轮询到 a）
        let other = route_account(&state, Some("sess-bbbb2222")).unwrap();
        assert_eq!(other.user_id, "b");
        // 同会话再来一次仍是 b
        assert_eq!(route_account(&state, Some("sess-aaaa1111")).unwrap().user_id, "b");
    }

    /// 优先策略：会话已绑定账户后保持粘滞，不因其他账户余量更多而漂移。
    #[test]
    fn priority_keeps_session_sticky_even_if_richer_account_exists() {
        // 会话已绑定 a，即使 b 余量更多也不切换（保护上游缓存）
        let state = state_with(vec![acct("a"), acct("b")], "priority", "");
        put_quota(&state, "a", Some((10.0, 50.0)), None, 10.0, 9.0);
        put_quota(&state, "b", Some((0.5, 50.0)), None, 10.0, 9.5);
        // 首次（无绑定）应选余量最多的 b
        assert_eq!(route_account(&state, Some("sess-cccc3333")).unwrap().user_id, "b");
        // 手工把该会话绑定到 a，后续保持 a（不因 b 更空而漂移）
        state.account_bindings.lock().unwrap().insert(
            "sess-cccc3333".into(),
            crate::proxy::state::AccountBinding { user_id: "a".into(), bound_at: crate::proxy::state::now_millis() },
        );
        assert_eq!(route_account(&state, Some("sess-cccc3333")).unwrap().user_id, "a");
    }

    /// 优先策略：绑定账户额度耗尽时自动切到余量最多者，并更新绑定。
    #[test]
    fn priority_switches_when_bound_account_exhausted() {
        // 绑定的 a 额度耗尽（5h 用满）→ 自动切到余量最多的 b
        let state = state_with(vec![acct("a"), acct("b")], "priority", "");
        put_quota(&state, "a", Some((50.0, 50.0)), None, 10.0, 5.0);
        put_quota(&state, "b", Some((5.0, 50.0)), None, 10.0, 9.0);
        state.account_bindings.lock().unwrap().insert(
            "sess-dddd4444".into(),
            crate::proxy::state::AccountBinding { user_id: "a".into(), bound_at: crate::proxy::state::now_millis() },
        );
        assert_eq!(route_account(&state, Some("sess-dddd4444")).unwrap().user_id, "b");
        // 新绑定已落到 b
        let bound = state.account_bindings.lock().unwrap().get("sess-dddd4444").map(|b| b.user_id.clone());
        assert_eq!(bound.as_deref(), Some("b"));
    }

    /// 优先策略：指定账户耗尽时退回到余量最多的其他账户。
    #[test]
    fn priority_falls_back_to_max_remaining_when_preferred_exhausted() {
        // 指定的 a 已耗尽 → 退回到余量最多的 c
        let state = state_with(vec![acct("a"), acct("c"), acct("b")], "priority", "a");
        put_quota(&state, "a", Some((50.0, 50.0)), None, 10.0, 0.0);
        put_quota(&state, "b", Some((20.0, 50.0)), None, 10.0, 6.0);
        put_quota(&state, "c", Some((2.0, 50.0)), None, 10.0, 9.8);
        assert_eq!(route_account(&state, Some("sess-eeee5555")).unwrap().user_id, "c");
    }

    /// 轮询策略：忽略粘滞绑定与额度，仅按顺序轮转，不产生新绑定。
    #[test]
    fn rr_strategy_ignores_bindings_and_rotates() {
        // round_robin：保持轮询语义，不看额度也不粘滞
        let state = state_with(vec![acct("a"), acct("b")], "round_robin", "b");
        put_quota(&state, "a", Some((50.0, 50.0)), None, 10.0, 0.0);
        let got: Vec<String> = (0..4)
            .map(|_| route_account(&state, Some("sess-ffff6666")).unwrap().user_id)
            .collect();
        assert_eq!(got, vec!["a", "b", "a", "b"]);
        assert!(state.account_bindings.lock().unwrap().is_empty());
    }

    /// 优先策略：无可用账户时返回 None。
    #[test]
    fn priority_empty_accounts_returns_none() {
        let state = state_with(vec![], "priority", "x");
        assert_eq!(route_account(&state, Some("sess-gggg7777")), None);
    }

    /// 旧版纯字符串 key 数组可反序列化为 Account（userId 为派生占位）。
    #[test]
    fn legacy_string_account_deser() {
        // 旧版纯字符串 key 数组应反序列化为 Account（user_id 为派生占位）
        let cfg: Config = serde_json::from_str(r#"{"cc_accounts":["user_abc"]}"#).unwrap();
        assert_eq!(cfg.cc_accounts.len(), 1);
        assert_eq!(cfg.cc_accounts[0].key, "user_abc");
        assert!(cfg.cc_accounts[0].user_id.starts_with("legacy-"));
        assert_eq!(cfg.cc_accounts[0].source, "manual");
    }
}
