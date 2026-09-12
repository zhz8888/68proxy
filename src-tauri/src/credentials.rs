//! 凭据管理：CC 上游账户 key 列表 + 本地转发鉴权 key，统一存于 SQLite `settings` 表。
//! config.json 不再承载 key。
//!
//! - **CC 账户**（`user_` 开头）：可配置多个，请求按轮询切换使用，分散单账户限流/配额压力。
//!   账户列表以 `AppState.config` 为内存真相源（转发热路径零 IO），写库由本模块负责。
//! - **本地转发 key**（`sk_` 开头）：仅本机服务鉴权用，客户端统一用它接入本地代理，
//!   可一键随机生成；该 key 不会发送给 CC 上游。

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

/// 从设置库读取 CC 账户列表（去空白、过滤空 key）。
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

/// 新增一个 CC 账户：以 `user_id` 为唯一标识，同 userId 已存在时更新其 key 与显示名
/// （视为同一账户重新登录/换 key），否则追加。落库后返回更新后的账户列表
/// （调用方负责同步到 AppState.config）。
pub fn add_account(conn: &Connection, acct: &Account) -> Result<Vec<Account>, String> {
    let key = acct.key.trim().to_string();
    if key.is_empty() {
        return Err("账户 key 不能为空".into());
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

/// 移除指定下标的 CC 账户（0 起）；下标越界时返回错误。
/// 返回更新后的账户列表（调用方负责同步到 AppState.config）。
pub fn remove_account_at(conn: &Connection, index: usize) -> Result<Vec<Account>, String> {
    let mut accounts = load_accounts(conn)?;
    if index >= accounts.len() {
        return Err(format!("账户下标越界: {index}"));
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
        return Err("显示名不能为空".into());
    }
    let mut accounts = load_accounts(conn)?;
    let Some(existing) = accounts.iter_mut().find(|a| a.user_id == user_id) else {
        return Err(format!("未找到 userId 为 {user_id} 的账户"));
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

/// 从 AppState 的配置读取 CC 账户列表。
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

/// 用 API Key 调用上游 `/alpha/whoami` 验证有效性并取回账户身份（userId/userName）。
///
/// 成功返回 `(userId, userName)`；401 表示 key 无效，其他状态/网络错误给出中文描述。
pub async fn verify_account_key(api_base: &str, api_key: &str) -> Result<(String, String), String> {
    let client = reqwest::Client::builder()
        .connect_timeout(std::time::Duration::from_secs(10))
        .build()
        .map_err(|e| format!("构建请求客户端失败: {e}"))?;
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
    .map_err(|_| "验证请求超时，请检查网络".to_string())?
    .map_err(|e| format!("验证请求失败: {e}"))?;

    match res.status().as_u16() {
        200 => {
            let v: serde_json::Value = res
                .json()
                .await
                .map_err(|e| format!("解析 whoami 响应失败: {e}"))?;
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
                return Err("whoami 响应缺少用户 id，请重试或改用浏览器登录".into());
            }
            Ok((user_id, user_name))
        }
        401 => Err("该 API Key 无效（未授权），请检查是否正确".into()),
        s => Err(format!("上游验证失败（HTTP {s}），请稍后重试")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::proxy::config::Config;
    use crate::proxy::settings::init_settings_on;
    use crate::proxy::state::AppState;

    fn temp_conn() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        init_settings_on(&conn).unwrap();
        conn
    }

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

    #[test]
    fn round_robin_empty() {
        let state = AppState::new(Config::default());
        assert_eq!(next_account(&state), None);
    }

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
