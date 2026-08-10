use std::path::Path;
use std::sync::{Mutex, OnceLock};

use crate::proxy::config::Config;

/// 内存缓存：避免每次请求都读取配置文件。
static CACHE: OnceLock<Mutex<Option<String>>> = OnceLock::new();

fn cache() -> &'static Mutex<Option<String>> {
    CACHE.get_or_init(|| Mutex::new(None))
}

pub fn cached_key() -> Option<String> {
    cache().lock().unwrap().clone()
}

fn config_at(path: &Path) -> Config {
    Config::load(path)
}

pub fn load_api_key(path: &Path) -> Result<Option<String>, String> {
    let key = config_at(path).api_key.trim().to_string();
    let key = if key.is_empty() { None } else { Some(key) };
    *cache().lock().unwrap() = key.clone();
    Ok(key)
}

pub fn save_api_key(path: &Path, key: &str) -> Result<(), String> {
    let key = key.trim().to_string();
    let mut cfg = config_at(path);
    cfg.api_key = key.clone();
    cfg.save(path)?;
    *cache().lock().unwrap() = Some(key);
    Ok(())
}

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
