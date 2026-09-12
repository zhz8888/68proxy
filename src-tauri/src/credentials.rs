//! 凭据管理：CC 上游账户 key 列表 + 本地转发鉴权 key，统一存于 SQLite `settings` 表。
//! config.json 不再承载 key。
//!
//! - **CC 账户**（`user_` 开头）：可配置多个，请求按轮询切换使用，分散单账户限流/配额压力。
//!   账户列表以 `AppState.config` 为内存真相源（转发热路径零 IO），写库由本模块负责。
//! - **本地转发 key**（`sk_` 开头）：仅本机服务鉴权用，客户端统一用它接入本地代理，
//!   可一键随机生成；该 key 不会发送给 CC 上游。

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

/// 从设置库读取 CC 账户 key 列表（去空白、过滤空值）。
pub fn load_accounts(conn: &Connection) -> Result<Vec<String>, String> {
    Ok(super::proxy::settings::load_config(conn)
        .cc_accounts
        .into_iter()
        .map(|k| k.trim().to_string())
        .filter(|k| !k.is_empty())
        .collect())
}

/// 新增一个 CC 账户 key（去空白后写入账户列表并落库）；已存在时静默跳过。
/// 返回更新后的账户列表（调用方负责同步到 AppState.config）。
pub fn add_account(conn: &Connection, key: &str) -> Result<Vec<String>, String> {
    let key = key.trim().to_string();
    if key.is_empty() {
        return Err("账户 key 不能为空".into());
    }
    let mut accounts = load_accounts(conn)?;
    if !accounts.contains(&key) {
        accounts.push(key);
        save_accounts(conn, &accounts)?;
    }
    Ok(accounts)
}

/// 移除指定下标的 CC 账户 key（0 起）；下标越界时返回错误。
/// 返回更新后的账户列表（调用方负责同步到 AppState.config）。
pub fn remove_account_at(conn: &Connection, index: usize) -> Result<Vec<String>, String> {
    let mut accounts = load_accounts(conn)?;
    if index >= accounts.len() {
        return Err(format!("账户下标越界: {index}"));
    }
    accounts.remove(index);
    save_accounts(conn, &accounts)?;
    Ok(accounts)
}

/// 把账户列表写入设置库（add/remove 落库用）。
fn save_accounts(conn: &Connection, accounts: &[String]) -> Result<(), String> {
    let mut cfg = super::proxy::settings::load_config(conn);
    cfg.cc_accounts = accounts.to_vec();
    super::proxy::settings::save_config(conn, &cfg)
}

/// 从 AppState 的配置读取 CC 账户列表。
pub fn accounts_from_state(state: &crate::proxy::state::AppState) -> Vec<String> {
    state.config.read().unwrap().cc_accounts.clone()
}

/// 掩码显示：user_ab12…cd34（长度不足时整体打点）。
pub fn mask_key(key: &str) -> String {
    if key.len() <= 8 {
        "••••".to_string()
    } else {
        format!("{}…{}", &key[..5], &key[key.len() - 4..])
    }
}

/// 从账户列表轮询取下一个账户 key：游标递增取模，多账户交替使用。
///
/// 账户列表为空时返回 None，单账户时恒返回该账户。游标为进程级静态计数，
/// 不同 AppState 实例（如测试）各自基于自己的列表取模，互不影响。
pub fn next_account(state: &crate::proxy::state::AppState) -> Option<String> {
    let accounts = accounts_from_state(state);
    if accounts.is_empty() {
        return None;
    }
    let idx = ROUND_ROBIN.fetch_add(1, Ordering::Relaxed) % accounts.len();
    Some(accounts[idx].clone())
}

/// 进程级轮询游标（静态计数，递增取模即得账户下标）。
static ROUND_ROBIN: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

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
        add_account(&conn, "user_one").unwrap();
        add_account(&conn, "user_two").unwrap();
        add_account(&conn, "user_one").unwrap(); // 重复添加被跳过
        let list = load_accounts(&conn).unwrap();
        assert_eq!(list, vec!["user_one".to_string(), "user_two".to_string()]);
        remove_account_at(&conn, 0).unwrap();
        assert_eq!(load_accounts(&conn).unwrap(), vec!["user_two".to_string()]);
        assert!(remove_account_at(&conn, 5).is_err()); // 越界报错
    }

    #[test]
    fn round_robin_rotates() {
        // 构造带两个账户的 state：a → b → a → b
        let state = AppState::new(Config::default());
        *state.config.write().unwrap() = Config {
            cc_accounts: vec!["user_a".into(), "user_b".into()],
            ..Config::default()
        };
        let mut got = Vec::new();
        for _ in 0..4 {
            got.push(next_account(&state).unwrap());
        }
        assert_eq!(got, vec!["user_a", "user_b", "user_a", "user_b"]);
    }

    #[test]
    fn round_robin_empty() {
        let state = AppState::new(Config::default());
        assert_eq!(next_account(&state), None);
    }
}
