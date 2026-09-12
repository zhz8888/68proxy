use serde::{Deserialize, Deserializer, Serialize};
use std::path::Path;

use super::log;

/// CC 上游账户。`user_id` 为唯一标识（whoami/OAuth 回传），同名账户重新登录换 key 时
/// 视为同一账户并更新 key；`user_name` 为显示名，可自定义，默认取 API 回传值。
///
/// 反序列化兼容旧格式：既接受 `{key, user_id, user_name, source, added_at}` 对象，
/// 也接受旧版纯字符串 key（迁成 `Account{ key, user_id: 派生占位, source: "manual" }`）。
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(default)]
pub struct Account {
    /// 上游 API key（user_ 开头），转发 Bearer 与伪造头用。
    pub key: String,
    /// 账户唯一标识（whoami 的 user.id / OAuth 回传 userId），运行时键控键。
    pub user_id: String,
    /// 显示名（可自定义，默认 API 回传 userName）。
    pub user_name: String,
    /// 来源：oauth / manual。
    pub source: String,
    /// 添加时间（Unix 秒）。
    pub added_at: u64,
}

impl Default for Account {
    fn default() -> Self {
        Self {
            key: String::new(),
            user_id: String::new(),
            user_name: String::new(),
            source: "manual".into(),
            added_at: 0,
        }
    }
}

/// 由 key 派生一个稳定的占位 user_id（用于旧版 key 迁移后，尚未 whoami 补全时）。
pub fn legacy_user_id(key: &str) -> String {
    use sha2::{Digest, Sha256};
    let mut h = Sha256::new();
    h.update(key.as_bytes());
    let hex = h.finalize();
    format!("legacy-{}", &hex[..8].iter().map(|b| format!("{b:02x}")).collect::<String>())
}

impl<'de> Deserialize<'de> for Account {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        #[derive(Deserialize)]
        #[serde(untagged)]
        enum Raw {
            Str(String),
            Obj {
                key: String,
                #[serde(default)]
                user_id: String,
                #[serde(default)]
                user_name: String,
                #[serde(default = "default_source")]
                source: String,
                #[serde(default)]
                added_at: u64,
            },
        }
        fn default_source() -> String {
            "manual".into()
        }
        match Raw::deserialize(deserializer)? {
            Raw::Str(k) => {
                let id = legacy_user_id(&k);
                Ok(Account {
                    key: k,
                    user_id: id,
                    user_name: String::new(),
                    source: "manual".into(),
                    added_at: 0,
                })
            }
            Raw::Obj {
                key,
                user_id,
                user_name,
                source,
                added_at,
            } => {
                let resolved_user_id = if user_id.is_empty() {
                    legacy_user_id(&key)
                } else {
                    user_id
                };
                Ok(Account {
                    key,
                    user_id: resolved_user_id,
                    user_name,
                    source,
                    added_at,
                })
            }
        }
    }
}

/// 代理配置，支持环境变量覆写。
///
/// `#[serde(default)]` 使缺失字段的旧配置文件也能正常反序列化。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    // 注意：旧版配置字段 `api_key` 已拆分为「CC 账户列表」与「本地转发 key」两部分，
    // 由 `migrate_legacy` 在反序列化后迁移，详见该函数。
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
    /// CC 上游账户列表（user_ 开头），请求按轮询切换使用；可空但启动代理需至少一个。
    pub cc_accounts: Vec<Account>,
    /// 本地转发鉴权 key（sk_ 开头，仅本机服务鉴权用，不发给 CC 上游）。
    pub local_api_key: String,
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
            cc_accounts: Vec::new(),
            local_api_key: String::new(),
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
            Ok(text) => {
                let v: serde_json::Value =
                    serde_json::from_str(&text).unwrap_or_else(|e| {
                        log::warn(&format!("配置解析失败，使用默认值: {e}"));
                        serde_json::json!({})
                    });
                let legacy = v
                    .get("api_key")
                    .and_then(|k| k.as_str())
                    .map(|s| s.to_string());
                let mut cfg: Config = serde_json::from_value(v).unwrap_or_default();
                cfg.migrate_legacy(legacy.as_deref());
                cfg
            }
            Err(_) => Config::default(),
        }
    }

    /// 兼容旧版配置迁移：旧字段 `api_key`（单个 user_ key）已拆分为账户列表 + 本地 key。
    ///
    /// 若账户列表为空且旧 key 非空，则把旧 key 作为首个 CC 账户迁入；本地 key 不迁移
    /// （由 UI 随机生成）。调用方（settings::load_config / load_file）在反序列化后调用。
    pub fn migrate_legacy(&mut self, legacy_api_key: Option<&str>) {
        if self.cc_accounts.is_empty() {
            if let Some(k) = legacy_api_key.filter(|k| !k.trim().is_empty()) {
                let k = k.trim().to_string();
                self.cc_accounts.push(Account {
                    user_id: legacy_user_id(&k),
                    key: k,
                    ..Account::default()
                });
            }
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn account_string_and_object_deser() {
        // 旧版纯字符串 key 数组 → Account（user_id 派生占位）
        let cfg: Config = serde_json::from_str(r#"{"cc_accounts":["user_abc"]}"#).unwrap();
        assert_eq!(cfg.cc_accounts.len(), 1);
        assert_eq!(cfg.cc_accounts[0].key, "user_abc");
        assert!(cfg.cc_accounts[0].user_id.starts_with("legacy-"));
        assert_eq!(cfg.cc_accounts[0].source, "manual");

        // 新版对象 → 原样反序列化
        let cfg2: Config = serde_json::from_str(
            r#"{"cc_accounts":[{"key":"user_xyz","user_id":"id_1","user_name":"小明","source":"oauth","added_at":123}]}"#,
        )
        .unwrap();
        assert_eq!(cfg2.cc_accounts[0].user_id, "id_1");
        assert_eq!(cfg2.cc_accounts[0].user_name, "小明");
        assert_eq!(cfg2.cc_accounts[0].source, "oauth");
        assert_eq!(cfg2.cc_accounts[0].added_at, 123);
    }

    #[test]
    fn legacy_api_key_migrates_to_account() {
        let mut cfg: Config = serde_json::from_str(r#"{"api_key":"user_legacy"}"#).unwrap();
        cfg.migrate_legacy(Some("user_legacy"));
        assert_eq!(cfg.cc_accounts.len(), 1);
        assert_eq!(cfg.cc_accounts[0].key, "user_legacy");
        assert!(cfg.cc_accounts[0].user_id.starts_with("legacy-"));
        assert_eq!(cfg.cc_accounts[0].source, "manual");
        // 已有账户时不迁移旧 key
        let mut cfg2: Config = serde_json::from_str(
            r#"{"cc_accounts":[{"key":"user_a","user_id":"id_a"}]}"#,
        )
        .unwrap();
        cfg2.migrate_legacy(Some("user_legacy"));
        assert_eq!(cfg2.cc_accounts.len(), 1);
        assert_eq!(cfg2.cc_accounts[0].key, "user_a");
    }

    #[test]
    fn legacy_user_id_stable() {
        assert_eq!(legacy_user_id("user_abc"), legacy_user_id("user_abc"));
        assert_ne!(legacy_user_id("user_abc"), legacy_user_id("user_def"));
        assert!(legacy_user_id("user_abc").starts_with("legacy-"));
    }
}
