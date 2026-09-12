//! API Key 凭据管理：设置统一存于 SQLite `settings` 表（键 `api_key`），
//! 并维护内存缓存供代理转发时快速取用。config.json 不再承载 Key。

use rusqlite::Connection;
use std::sync::{Mutex, OnceLock};

/// 内存缓存：避免每次请求都读取数据库。
static CACHE: OnceLock<Mutex<Option<String>>> = OnceLock::new();

/// 获取全局缓存的单例句柄（首次调用时初始化）。
fn cache() -> &'static Mutex<Option<String>> {
    CACHE.get_or_init(|| Mutex::new(None))
}

/// 读取内存缓存中的 API Key（不读库）；未设置过 Key 时返回 None。
pub fn cached_key() -> Option<String> {
    cache().lock().unwrap().clone()
}

/// 从设置库读取 API Key 并刷新内存缓存；空字符串视为未设置（返回 None）。
pub fn load_api_key(conn: &Connection) -> Result<Option<String>, String> {
    let key = super::proxy::settings::load_config(conn).api_key.trim().to_string();
    let key = if key.is_empty() { None } else { Some(key) };
    *cache().lock().unwrap() = key.clone();
    Ok(key)
}

/// 保存 API Key：去除首尾空白后写入设置库，并同步更新内存缓存。
pub fn save_api_key(conn: &Connection, key: &str) -> Result<(), String> {
    let key = key.trim().to_string();
    let mut cfg = super::proxy::settings::load_config(conn);
    cfg.api_key = key.clone();
    super::proxy::settings::save_config(conn, &cfg)?;
    *cache().lock().unwrap() = Some(key);
    Ok(())
}

/// 删除已保存的 API Key：清空设置库中的字段并清除内存缓存。
pub fn delete_api_key(conn: &Connection) -> Result<(), String> {
    let mut cfg = super::proxy::settings::load_config(conn);
    cfg.api_key = String::new();
    super::proxy::settings::save_config(conn, &cfg)?;
    *cache().lock().unwrap() = None;
    Ok(())
}

/// 掩码显示：user_ab12…cd34
pub fn mask_key(key: &str) -> String {
    if key.len() <= 8 {
        "••••".to_string()
    } else {
        format!("{}…{}", &key[..5], &key[key.len() - 4..])
    }
}
