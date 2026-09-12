use serde::{Deserialize, Serialize};
use std::path::Path;

use super::log;

/// 代理配置，支持环境变量覆写。
///
/// `#[serde(default)]` 使缺失字段的旧配置文件也能正常反序列化。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    /// 本地代理监听端口。
    pub port: u16,
    /// 监听地址（IP，如 0.0.0.0 / 127.0.0.1）。
    pub host: String,
    /// CC 上游 API 根地址（含协议前缀）。
    pub api_base: String,
    /// 项目 slug，保留字段（实际转发时使用会话派生的伪造 slug）。
    pub project_slug: String,
    /// 日志文件路径，空字符串表示不写文件。
    pub log_file: String,
    /// 日志级别：debug / info / warn / error。
    pub log_level: String,
    /// 是否从 Provider API 动态拉取模型列表（关闭时使用内置硬编码列表）。
    pub use_provider_models: bool,
    /// 模型列表缓存刷新间隔（毫秒）。
    pub model_refresh_interval_ms: u64,
    /// 应用启动后是否自动开启代理服务。
    pub auto_start_proxy: bool,
    /// 启动时是否显示主窗口。
    pub show_window_on_start: bool,
    /// 是否开机自启动（操作系统级登录项）。
    pub autostart: bool,
    /// 关闭窗口时隐藏到托盘而不是退出（仅桌面行为）。
    pub close_to_tray: bool,
    /// 是否启用 token 用量统计（关闭后不再记录新用量，历史数据保留）。
    pub usage_enabled: bool,
    /// 用量明细保留天数，0 表示永久保留；超过部分在记录时自动清理。
    pub usage_retention_days: u32,
    /// 本地明文保存的 API Key（user_ 开头），随配置文件读写。
    pub api_key: String,
    /// 无 system prompt 时是否发空格占位（阻止 CC 上游注入默认提示词）。
    pub empty_system_placeholder: bool,
    /// 是否启用 ZDR 模式（向 CC 上游发送 x-cmd-zdr: 1 请求头）。
    pub zdr: bool,
    /// 请求体大小上限（MB），超限请求返回 413（连接保持排空可复用）。
    pub max_body_mb: u32,
    /// 下游写缓冲背压僵死看门狗（毫秒），0 表示禁用（不主动断开僵死客户端）。
    pub client_drain_timeout_ms: u64,
    /// 进程内在途请求上限，0 表示不限；超限返回 503 + Retry-After。
    pub max_inflight: u32,
}

impl Default for Config {
    /// 返回内置默认配置（端口 3050、上游 api.commandcode.ai 等）。
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
            usage_enabled: true,
            usage_retention_days: 0,
            api_key: String::new(),
            empty_system_placeholder: true,
            zdr: false,
            max_body_mb: 10,
            client_drain_timeout_ms: 0,
            max_inflight: 0,
        }
    }
}

impl Config {
    /// 校验配置合法性（端口范围、监听地址、api_base 协议前缀、日志级别枚举）。
    /// 返回 `Err(中文错误描述)` 表示不合法。
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

    /// 从 JSON 文件加载配置（**不**应用环境变量覆写）；文件缺失或解析失败时回退默认值。
    ///
    /// 用于 SQLite 首次迁移，避免环境变量值被写入设置表。
    pub fn load_file(path: &Path) -> Config {
        match std::fs::read_to_string(path) {
            Ok(text) => serde_json::from_str::<Config>(&text).unwrap_or_else(|e| {
                log::warn(&format!("配置解析失败，使用默认值: {e}"));
                Config::default()
            }),
            Err(_) => Config::default(),
        }
    }

    /// 将配置以缩进 JSON 写入 `path`，父目录不存在时自动创建。
    pub fn save(&self, path: &Path) -> Result<(), String> {
        let text = serde_json::to_string_pretty(self)
            .map_err(|e| format!("配置序列化失败: {e}"))?;
        if let Some(dir) = path.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        std::fs::write(path, text).map_err(|e| format!("配置写入失败: {e}"))
    }

    /// 用环境变量覆写对应字段：PORT / HOST / CC_API_BASE / PROJECT_SLUG / LOG_FILE /
    /// CC_USE_PROVIDER_MODELS（仅显式 "false" 时关闭）/ CC_EMPTY_SYSTEM_PLACEHOLDER
    /// （仅显式 "false" 时关闭）/ CMD_ZDR（仅显式 "1" 或 "true" 时开启）/
    /// CC_MAX_BODY_MB / CC_CLIENT_DRAIN_TIMEOUT_MS / CC_MAX_INFLIGHT。
    pub fn apply_env(&mut self) {
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
        if let Ok(v) = std::env::var("CC_EMPTY_SYSTEM_PLACEHOLDER") {
            self.empty_system_placeholder = v != "false";
        }
        if let Ok(v) = std::env::var("CMD_ZDR") {
            self.zdr = matches!(v.as_str(), "1" | "true");
        }
        if let Ok(v) = std::env::var("CC_MAX_BODY_MB") {
            if let Ok(p) = v.parse::<u32>() {
                if p > 0 {
                    self.max_body_mb = p;
                }
            }
        }
        if let Ok(v) = std::env::var("CC_CLIENT_DRAIN_TIMEOUT_MS") {
            if let Ok(p) = v.parse::<u64>() {
                self.client_drain_timeout_ms = p;
            }
        }
        if let Ok(v) = std::env::var("CC_MAX_INFLIGHT") {
            if let Ok(p) = v.parse::<u32>() {
                self.max_inflight = p;
            }
        }
    }
}
