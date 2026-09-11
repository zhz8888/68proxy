//! API Key 凭据管理：负责从配置文件读取/保存/删除上游 Key，
//! 并在内存中缓存最近一次加载的值，供代理转发时快速取用。

use std::path::Path;
use std::sync::{Mutex, OnceLock};

use crate::proxy::config::Config;

/// 内存缓存：避免每次请求都读取配置文件。
static CACHE: OnceLock<Mutex<Option<String>>> = OnceLock::new();

/// 获取全局缓存的单例句柄（首次调用时初始化）。
fn cache() -> &'static Mutex<Option<String>> {
    CACHE.get_or_init(|| Mutex::new(None))
}

/// 读取内存缓存中的 API Key（不读磁盘）；未保存过 Key 时返回 None。
pub fn cached_key() -> Option<String> {
    cache().lock().unwrap().clone()
}

/// 从指定路径加载完整配置文件。
fn config_at(path: &Path) -> Config {
    Config::load(path)
}

/// 从配置文件读取 API Key 并刷新内存缓存；空字符串视为未设置（返回 None）。
pub fn load_api_key(path: &Path) -> Result<Option<String>, String> {
    let key = config_at(path).api_key.trim().to_string();
    let key = if key.is_empty() { None } else { Some(key) };
    *cache().lock().unwrap() = key.clone();
    Ok(key)
}

/// 保存 API Key：去除首尾空白后写入配置文件，并同步更新内存缓存。
pub fn save_api_key(path: &Path, key: &str) -> Result<(), String> {
    let key = key.trim().to_string();
    let mut cfg = config_at(path);
    cfg.api_key = key.clone();
    cfg.save(path)?;
    *cache().lock().unwrap() = Some(key);
    Ok(())
}

/// 删除已保存的 API Key：清空配置文件中的字段并清除内存缓存。
pub fn delete_api_key(path: &Path) -> Result<(), String> {
    let mut cfg = config_at(path);
    cfg.api_key = String::new();
    cfg.save(path)?;
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
