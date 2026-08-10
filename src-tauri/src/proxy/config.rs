use serde::{Deserialize, Serialize};
use std::path::Path;

use super::log;

/// 代理配置，支持环境变量覆写。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    pub port: u16,
    pub host: String,
    pub api_base: String,
    pub project_slug: String,
    pub log_file: String,
    pub log_level: String,
    pub use_provider_models: bool,
    pub model_refresh_interval_ms: u64,
    pub auto_start_proxy: bool,
    pub show_window_on_start: bool,
    pub autostart: bool,
    /// 关闭窗口时隐藏到托盘而不是退出（仅桌面行为）。
    pub close_to_tray: bool,
    /// 本地明文保存的 API Key（user_ 开头），随配置文件读写。
    pub api_key: String,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            port: 3050,
            host: "0.0.0.0".into(),
            api_base: "https://api.commandcode.ai".into(),
            project_slug: "cc-proxy".into(),
            log_file: String::new(),
            log_level: "info".into(),
            use_provider_models: true,
            model_refresh_interval_ms: 300_000,
            auto_start_proxy: false,
            show_window_on_start: true,
            autostart: false,
            close_to_tray: true,
            api_key: String::new(),
        }
    }
}

impl Config {
    pub fn validate(&self) -> Result<(), String> {
        if !(1..=65535).contains(&self.port) {
            return Err("端口必须在 1-65535 之间".into());
        }
        if self.host.trim().is_empty() {
            return Err("监听地址不能为空".into());
        }
        if !self.api_base.starts_with("http://") && !self.api_base.starts_with("https://") {
            return Err("上游 API 地址必须以 http:// 或 https:// 开头".into());
        }
        if !matches!(self.log_level.as_str(), "debug" | "info" | "warn" | "error") {
            return Err("日志级别只能是 debug/info/warn/error".into());
        }
        Ok(())
    }

    pub fn load(path: &Path) -> Config {
        let mut cfg = match std::fs::read_to_string(path) {
            Ok(text) => serde_json::from_str::<Config>(&text).unwrap_or_else(|e| {
                log::warn(&format!("配置解析失败，使用默认值: {e}"));
                Config::default()
            }),
            Err(_) => Config::default(),
        };
        cfg.apply_env();
        cfg
    }

    pub fn save(&self, path: &Path) -> Result<(), String> {
        let text = serde_json::to_string_pretty(self)
            .map_err(|e| format!("配置序列化失败: {e}"))?;
        if let Some(dir) = path.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        std::fs::write(path, text).map_err(|e| format!("配置写入失败: {e}"))
    }

    fn apply_env(&mut self) {
        if let Ok(v) = std::env::var("PORT") {
            if let Ok(p) = v.parse::<u16>() {
                self.port = p;
            }
        }
        if let Ok(v) = std::env::var("HOST") {
            self.host = v;
        }
        if let Ok(v) = std::env::var("CC_API_BASE") {
            self.api_base = v;
        }
        if let Ok(v) = std::env::var("PROJECT_SLUG") {
            self.project_slug = v;
        }
        if let Ok(v) = std::env::var("LOG_FILE") {
            self.log_file = v;
        }
        if let Ok(v) = std::env::var("CC_USE_PROVIDER_MODELS") {
            self.use_provider_models = v != "false";
        }
    }
}
