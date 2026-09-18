use serde::{Deserialize, Deserializer, Serialize};
use std::path::Path;

use super::log;

/// Command Code 上游账户。`user_id` 为唯一标识（whoami/OAuth 回传），同名账户重新登录换 key 时
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
    /// 返回空账户（来源 manual，无添加时间）。
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
    /// 兼容两种形态：纯字符串（旧版单 key 配置）或完整对象；字符串形态按
    /// `legacy_user_id` 派生占位 userId 并补全默认来源与时间。
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        /// 反序列化中间形态：旧版纯字符串 key 或新版完整对象。
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
        /// `source` 字段的默认值（serde 不支持直接写字面量）。
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
    // 注意：旧版配置字段 `api_key` 已拆分为「Command Code 账户列表」与「本地转发 key」两部分，
    // 由 `migrate_legacy` 在反序列化后迁移，详见该函数。
    /// 本地代理监听端口。
    pub port: u16,
    /// 监听地址（IP，如 0.0.0.0 / 127.0.0.1）。
    pub host: String,
    /// Command Code 上游 API 根地址（含协议前缀）。
    pub api_base: String,
    /// 项目 slug，保留字段（实际转发时使用会话派生的伪造 slug）。
    pub project_slug: String,
    /// 日志文件路径，空字符串表示不写文件。
    pub log_file: String,
    /// 日志级别：debug / info / warn / error。
    pub log_level: String,
    /// 是否从 Provider API 动态拉取模型列表（关闭时使用内置硬编码列表）。
    pub use_provider_models: bool,
    /// 模型列表缓存刷新间隔（秒）。
    pub model_refresh_interval_secs: u64,
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
    /// Command Code 上游账户列表（user_ 开头），请求按轮询切换使用；可空但启动代理需至少一个。
    pub cc_accounts: Vec<Account>,
    /// 本地转发鉴权 key（sk_ 开头，仅本机服务鉴权用，不发给 Command Code 上游）。
    pub local_api_key: String,
    /// 无 system prompt 时是否发空格占位（阻止 Command Code 上游注入默认提示词）。
    pub empty_system_placeholder: bool,
    /// 是否启用 ZDR 模式（向 Command Code 上游发送 x-cmd-zdr: 1 请求头）。
    pub zdr: bool,
    /// 请求体大小上限（MB），超限请求返回 413（连接保持排空可复用）。
    pub max_body_mb: u32,
    /// 下游写缓冲背压僵死看门狗（毫秒），0 表示禁用（不主动断开僵死客户端）。
    pub client_drain_timeout_ms: u64,
    /// 流式响应两次上游数据之间的最大空闲时间（秒），0 表示不限（默认）。
    ///
    /// 上游对思考型模型在思考阶段完全不发事件（实测静默 50~55 秒，长思考可达 395 秒
    /// 以上），官方 CLI 对上游也不设任何 idle timeout；默认不限可避免把健康请求误判为
    /// 超时，仅在显式配置后才启用该兜底。
    pub stream_idle_timeout_secs: u64,
    /// 非流式响应等待上游数据的最大空闲时间（秒），0 表示不限（默认）。
    pub nonstream_idle_timeout_secs: u64,
    /// 进程内在途请求上限，0 表示不限；超限返回 503 + Retry-After。默认 32。
    pub max_inflight: u32,
    /// 账户使用策略：`round_robin`（轮询，默认）/ `priority`（优先消耗指定账户 + 会话粘滞）。
    pub account_strategy: String,
    /// 优先消耗的账户 userId（仅 `priority` 策略生效；空字符串表示自动选剩余额度最多者）。
    pub preferred_account_id: String,
    /// 界面主题：`system`（跟随系统，默认）/ `dark` / `light`。
    pub theme: String,
    /// 界面语言：`zh`（简体中文，默认）/ `en`（英文）。
    pub language: String,
    /// 出站代理模式：`none`（不走代理，默认）/ `system`（跟随系统环境变量）/ `custom`（自定义代理）。
    pub proxy_mode: String,
    /// 自定义代理类型：`socks5` / `http`（仅 custom 模式生效）。
    pub proxy_type: String,
    /// 自定义代理主机（仅 custom 模式生效）。
    pub proxy_host: String,
    /// 自定义代理端口（仅 custom 模式生效）。
    pub proxy_port: u16,
    /// 自定义代理认证用户名（可选，仅 custom 模式生效）。
    pub proxy_username: String,
    /// 自定义代理认证密码（可选，仅 custom 模式生效）。
    pub proxy_password: String,
    /// 信封 mode（/alpha/generate 请求体的顶层 mode 字段）。
    /// 服务端枚举（真机 400 报出）：agent | learning | custom-agent | custom-agent-create |
    /// title-gen | tool-desc | compact | vision；默认 agent。
    pub cli_mode: String,
    /// lifecycle metadata 的 mode —— 注意这是另一个枚举：interactive | non-interactive。
    pub cli_session_mode: String,
    /// 指纹盐：改这个值 = 让所有账户换一台设备（哈希阶段仍用 CLI 固定盐，见 fingerprint.rs）。
    pub fingerprint_salt: String,
    /// 伪造的项目目录（留空则用内置的 C:\Users\dev\projects\app）；
    /// 与 x-project-slug 同源，供多实例区分项目。
    pub device_project_dir: String,
}

impl Default for Config {
    /// 返回内置默认配置（端口 3050、上游 api.commandcode.ai 等）。
    fn default() -> Self {
        Self {
            port: 3050,
            // 默认只监听回环：0.0.0.0 会把带着账户凭据的转发口暴露到局域网，
            // 需要局域网访问时由用户在配置页显式改为 0.0.0.0。
            host: "127.0.0.1".into(),
            api_base: "https://api.commandcode.ai".into(),
            project_slug: "cc-proxy".into(),
            log_file: String::new(),
            log_level: "info".into(),
            use_provider_models: true,
            model_refresh_interval_secs: 300,
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
            // 默认不限：上游思考阶段可能静默数百秒而不发任何字节，设超时会误杀健康请求
            //（官方 CLI 对上游不设 idle timeout）。需要兜底时由用户在配置页显式填秒数。
            stream_idle_timeout_secs: 0,
            nonstream_idle_timeout_secs: 0,
            // 默认给一个正数上限：0（不限）会让并发请求无界占用内存与上游连接，
            // 用户仍可在配置页改为 0 表示不限。
            max_inflight: 32,
            account_strategy: "round_robin".into(),
            preferred_account_id: String::new(),
            theme: "system".into(),
            language: "zh".into(),
            proxy_mode: "none".into(),
            proxy_type: "socks5".into(),
            proxy_host: String::new(),
            proxy_port: 0,
            proxy_username: String::new(),
            proxy_password: String::new(),
            cli_mode: "agent".into(),
            cli_session_mode: "interactive".into(),
            fingerprint_salt: String::new(),
            device_project_dir: String::new(),
        }
    }
}

impl Config {
    /// `api_base` 是否允许：必须是 https 的 Command Code 官方域名，或是本机回环地址。
    ///
    /// 上游地址决定账户 key（`Authorization: Bearer`）会被发往何处。若不加约束，
    /// 一次 IPC 调用即可把上游指向攻击者主机，后续所有账户 key 都会外泄到那里。
    /// 回环地址（本地自建/测试）不构成外泄，放行；其他自定义主机需显式设置
    /// 环境变量 `CC_ALLOW_CUSTOM_API_BASE=1`。
    fn api_base_allowed(&self) -> bool {
        if std::env::var("CC_ALLOW_CUSTOM_API_BASE").as_deref() == Ok("1") {
            return true;
        }
        let rest = self.api_base.split("://").nth(1).unwrap_or("");
        let host_port = rest.split(['/', '?', '#']).next().unwrap_or("");
        let host = host_port
            .rsplit_once(':')
            .map(|(h, _)| h)
            .unwrap_or(host_port)
            .trim_start_matches('[')
            .trim_end_matches(']');
        let loopback = matches!(host, "127.0.0.1" | "localhost" | "::1");
        if loopback {
            return true;
        }
        self.api_base.starts_with("https://")
            && (host == "commandcode.ai" || host.ends_with(".commandcode.ai"))
    }

    /// 校验配置合法性（端口范围、监听地址、api_base 协议前缀、日志级别枚举）。
    /// 返回 `Err(消息码)`（形如 `err:<code>`，见 crate::i18n），由前端翻译为当前语言。
    pub fn validate(&self) -> Result<(), String> {
        if !(1..=65535).contains(&self.port) {
            return Err(crate::i18n::err("config_invalid_port"));
        }
        if self.host.trim().is_empty() {
            return Err(crate::i18n::err("config_invalid_host"));
        }
        if !self.api_base.starts_with("http://") && !self.api_base.starts_with("https://") {
            return Err(crate::i18n::err("config_invalid_api_base"));
        }
        if !self.api_base_allowed() {
            return Err(crate::i18n::err("config_invalid_api_base"));
        }
        if !matches!(self.log_level.as_str(), "debug" | "info" | "warn" | "error") {
            return Err(crate::i18n::err("config_invalid_log_level"));
        }
        if !matches!(self.account_strategy.as_str(), "round_robin" | "priority") {
            return Err(crate::i18n::err("config_invalid_strategy"));
        }
        if !matches!(self.theme.as_str(), "system" | "dark" | "light") {
            return Err(crate::i18n::err("config_invalid_theme"));
        }
        if !matches!(self.language.as_str(), "zh" | "en") {
            return Err(crate::i18n::err("config_invalid_language"));
        }
        if !matches!(self.proxy_mode.as_str(), "none" | "system" | "custom") {
            return Err(crate::i18n::err("config_invalid_proxy_mode"));
        }
        if !matches!(self.proxy_type.as_str(), "socks5" | "http") {
            return Err(crate::i18n::err("config_invalid_proxy_type"));
        }
        if self.proxy_mode == "custom" {
            if self.proxy_host.trim().is_empty() {
                return Err(crate::i18n::err("config_invalid_proxy_host"));
            }
            if !(1..=65535).contains(&self.proxy_port) {
                return Err(crate::i18n::err("config_invalid_proxy_port"));
            }
        }
        Ok(())
    }

    /// 账户使用策略是否为「优先消耗 + 会话粘滞」。
    pub fn is_priority_strategy(&self) -> bool {
        self.account_strategy == "priority"
    }

    /// 从 JSON 文件加载配置（**不**应用环境变量覆写）；文件缺失或解析失败时回退默认值。
    ///
    /// 用于 SQLite 首次迁移，避免环境变量值被写入设置表。
    pub fn load_file(path: &Path) -> Config {
        match std::fs::read_to_string(path) {
            Ok(text) => {
                let mut v: serde_json::Value =
                    serde_json::from_str(&text).unwrap_or_else(|e| {
                        log::warn(&format!(
                            "{}: {e}",
                            crate::i18n::pick("配置解析失败，使用默认值", "Failed to parse config, using defaults")
                        ));
                        serde_json::json!({})
                    });
                // 单位迁移须在反序列化前完成（详见函数注释）
                if let Some(obj) = v.as_object_mut() {
                    migrate_refresh_interval_unit(obj);
                }
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

    /// 将配置以缩进 JSON 写入 `path`，父目录不存在时自动创建。
    ///
    /// 凭据（本地转发 key 与 Command Code 账户）只存 SQLite settings 表，镜像文件不落盘明文：
    /// 写入前把这两项清空，避免 config.json 泄露 key（与 credentials 模块声明一致）。
    pub fn save(&self, path: &Path) -> Result<(), String> {
        let mut mirrored = self.clone();
        mirrored.local_api_key = String::new();
        mirrored.cc_accounts = Vec::new();
        let text = serde_json::to_string_pretty(&mirrored).map_err(|e| {
            format!(
                "{}: {e}",
                crate::i18n::pick("配置序列化失败", "Failed to serialize the config")
            )
        })?;
        if let Some(dir) = path.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        std::fs::write(path, text).map_err(|e| {
            format!(
                "{}: {e}",
                crate::i18n::pick("配置写入失败", "Failed to write the config")
            )
        })
    }

    /// 用环境变量覆写对应字段：PORT / HOST / CC_API_BASE / PROJECT_SLUG / LOG_FILE /
    /// CC_USE_PROVIDER_MODELS（仅显式 "false" 时关闭）/ CC_EMPTY_SYSTEM_PLACEHOLDER
    /// （仅显式 "false" 时关闭）/ CMD_ZDR（仅显式 "1" 或 "true" 时开启）/
    /// CC_MAX_BODY_MB / CC_CLIENT_DRAIN_TIMEOUT_MS / CC_STREAM_IDLE_SECS /
    /// CC_NONSTREAM_IDLE_SECS / CC_MAX_INFLIGHT。
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
        if let Ok(v) = std::env::var("CC_STREAM_IDLE_SECS") {
            if let Ok(p) = v.parse::<u64>() {
                self.stream_idle_timeout_secs = p;
            }
        }
        if let Ok(v) = std::env::var("CC_NONSTREAM_IDLE_SECS") {
            if let Ok(p) = v.parse::<u64>() {
                self.nonstream_idle_timeout_secs = p;
            }
        }
        if let Ok(v) = std::env::var("CC_MAX_INFLIGHT") {
            if let Ok(p) = v.parse::<u32>() {
                self.max_inflight = p;
            }
        }
        if let Ok(v) = std::env::var("CC_ACCOUNT_STRATEGY") {
            if matches!(v.as_str(), "round_robin" | "priority") {
                self.account_strategy = v;
            }
        }
        if let Ok(v) = std::env::var("CC_PREFERRED_ACCOUNT_ID") {
            self.preferred_account_id = v;
        }
        if let Ok(v) = std::env::var("CC_CLI_MODE") {
            self.cli_mode = v;
        }
        if let Ok(v) = std::env::var("CC_CLI_SESSION_MODE") {
            self.cli_session_mode = v;
        }
        if let Ok(v) = std::env::var("CC_FINGERPRINT_SALT") {
            self.fingerprint_salt = v;
        }
        if let Ok(v) = std::env::var("CC_DEVICE_PROJECT_DIR") {
            self.device_project_dir = v;
        }
    }

    /// 兼容旧版配置迁移：旧字段 `api_key`（单个 user_ key）已拆分为账户列表 + 本地 key。
    ///
    /// 若账户列表为空且旧 key 非空，则把旧 key 作为首个 Command Code 账户迁入；本地 key 不迁移
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
}

/// 把旧版毫秒单位的刷新间隔迁移为秒（在反序列化**之前**对原始对象调用）。
///
/// 旧配置写作 `model_refresh_interval_ms`（如 300000）；新字段为
/// `model_refresh_interval_secs`。仅当新字段缺失且旧字段存在时转换，随后删除旧字段
/// 以免残留误导。必须在反序列化前执行：否则 `#[serde(default)]` 会先填默认值，
/// 旧值无从还原。
pub fn migrate_refresh_interval_unit(obj: &mut serde_json::Map<String, serde_json::Value>) {
    if obj.contains_key("model_refresh_interval_secs") {
        obj.remove("model_refresh_interval_ms");
        return;
    }
    if let Some(ms) = obj.get("model_refresh_interval_ms").and_then(|v| v.as_u64()) {
        obj.insert(
            "model_refresh_interval_secs".into(),
            serde_json::json!(ms / 1000),
        );
    }
    obj.remove("model_refresh_interval_ms");
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 账户双形态反序列化：旧版纯字符串 key 派生占位 userId，新版对象原样解析。
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

    /// 旧版 `api_key` 迁移为首个账户；已有账户时跳过迁移。
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

    /// 占位 userId 派生稳定且对不同 key 不碰撞。
    #[test]
    fn legacy_user_id_stable() {
        assert_eq!(legacy_user_id("user_abc"), legacy_user_id("user_abc"));
        assert_ne!(legacy_user_id("user_abc"), legacy_user_id("user_def"));
        assert!(legacy_user_id("user_abc").starts_with("legacy-"));
    }

    /// 旧毫秒字段换算为秒并清理旧键；已是新字段时保原值；两者皆缺时不注入。
    #[test]
    fn refresh_interval_migrates_ms_to_secs() {
        // 旧配置写作毫秒字段：应换算为秒并删除旧键
        let mut obj = serde_json::json!({ "model_refresh_interval_ms": 300000 });
        migrate_refresh_interval_unit(obj.as_object_mut().unwrap());
        assert_eq!(obj["model_refresh_interval_secs"], serde_json::json!(300));
        assert!(obj.get("model_refresh_interval_ms").is_none());

        // 已是新字段时保持原值，仅清理可能残留的旧键
        let mut obj2 = serde_json::json!({ "model_refresh_interval_secs": 45, "model_refresh_interval_ms": 999 });
        migrate_refresh_interval_unit(obj2.as_object_mut().unwrap());
        assert_eq!(obj2["model_refresh_interval_secs"], serde_json::json!(45));

        // 两者都缺失：不注入字段，交给默认值
        let mut obj3 = serde_json::json!({ "port": 3050 });
        migrate_refresh_interval_unit(obj3.as_object_mut().unwrap());
        assert!(obj3.get("model_refresh_interval_secs").is_none());
        assert_eq!(obj3["port"], serde_json::json!(3050));
    }

    /// 端到端：旧毫秒配置落盘后经 load_file 读出为秒值。
    #[test]
    fn load_file_applies_unit_migration() {
        // 端到端：旧 json 落盘后经 load_file 应得到秒值
        let dir = std::env::temp_dir();
        let path = dir.join(format!("cc-config-refresh-{}.json", std::process::id()));
        let _ = std::fs::remove_file(&path);
        std::fs::write(&path, r#"{"model_refresh_interval_ms":120000}"#).unwrap();
        let cfg = Config::load_file(&path);
        assert_eq!(cfg.model_refresh_interval_secs, 120);
        let _ = std::fs::remove_file(&path);
    }

    /// 镜像文件不落盘明文凭据（local_api_key/cc_accounts 写空），非敏感字段正常持久化。
    #[test]
    fn save_mirror_strips_credentials() {
        // 镜像文件不能落盘明文 key：写盘内容中 local_api_key/cc_accounts 必须为空
        let dir = std::env::temp_dir();
        let path = dir.join(format!("cc-config-save-{}.json", std::process::id()));
        let _ = std::fs::remove_file(&path);

        let mut cfg = Config::default();
        cfg.port = 3456;
        cfg.local_api_key = "sk-secret".into();
        cfg.cc_accounts = vec![Account {
            key: "user_secret".into(),
            user_id: "id_1".into(),
            ..Account::default()
        }];
        cfg.save(&path).unwrap();

        let raw = std::fs::read_to_string(&path).unwrap();
        assert!(!raw.contains("sk-secret"), "镜像文件不应包含本地 key");
        assert!(!raw.contains("user_secret"), "镜像文件不应包含账户 key");
        // 非敏感字段仍正常持久化
        let loaded = Config::load_file(&path);
        assert_eq!(loaded.port, 3456);
        assert!(loaded.local_api_key.is_empty());
        assert!(loaded.cc_accounts.is_empty());
        let _ = std::fs::remove_file(&path);
    }

    /// validate 的全部非法分支：字段值逐一越界时应返回对应错误码。
    #[test]
    fn validate_rejects_each_invalid_field() {
        let c = Config::default();
        c.validate().unwrap(); // 默认配置合法
        let cases: &[(&str, fn(&mut Config))] = &[
            ("config_invalid_port", |c: &mut Config| c.port = 0),
            ("config_invalid_host", |c: &mut Config| c.host = "  ".into()),
            (
                "config_invalid_api_base",
                |c: &mut Config| c.api_base = "ftp://x".into(),
            ),
            (
                // 非官方域名的 https 上游同样拒绝：防止把账户 key 发往攻击者主机
                "config_invalid_api_base",
                |c: &mut Config| c.api_base = "https://evil.example.com".into(),
            ),
            (
                // 官方域名也必须是 https
                "config_invalid_api_base",
                |c: &mut Config| c.api_base = "http://api.commandcode.ai".into(),
            ),
            (
                "config_invalid_log_level",
                |c: &mut Config| c.log_level = "verbose".into(),
            ),
            (
                "config_invalid_strategy",
                |c: &mut Config| c.account_strategy = "random".into(),
            ),
            ("config_invalid_theme", |c: &mut Config| c.theme = "pink".into()),
            (
                "config_invalid_language",
                |c: &mut Config| c.language = "jp".into(),
            ),
            (
                "config_invalid_proxy_mode",
                |c: &mut Config| c.proxy_mode = "auto".into(),
            ),
            (
                "config_invalid_proxy_type",
                |c: &mut Config| c.proxy_type = "socks4".into(),
            ),
            (
                "config_invalid_proxy_host",
                |c: &mut Config| {
                    c.proxy_mode = "custom".into();
                    c.proxy_host = " ".into();
                    c.proxy_port = 1080;
                },
            ),
            (
                "config_invalid_proxy_port",
                |c: &mut Config| {
                    c.proxy_mode = "custom".into();
                    c.proxy_host = "127.0.0.1".into();
                    c.proxy_port = 0;
                },
            ),
        ];
        for (code, mutate) in cases {
            let mut c2 = Config::default();
            mutate(&mut c2);
            let err = c2.validate().unwrap_err();
            assert!(err.contains(code), "期望 {code}，实际 {err}");
        }
    }

    /// 环境变量覆写：合法值生效，非法值被忽略（回退配置原值）。
    #[test]
    fn apply_env_overrides_fields() {
        std::env::set_var("PORT", "4050");
        std::env::set_var("HOST", "127.0.0.1");
        std::env::set_var("CC_API_BASE", "https://env.example.com");
        std::env::set_var("PROJECT_SLUG", "env-slug");
        std::env::set_var("LOG_FILE", "/tmp/env.log");
        std::env::set_var("CC_USE_PROVIDER_MODELS", "false");
        std::env::set_var("CC_EMPTY_SYSTEM_PLACEHOLDER", "false");
        std::env::set_var("CMD_ZDR", "1");
        std::env::set_var("CC_MAX_BODY_MB", "32");
        std::env::set_var("CC_CLIENT_DRAIN_TIMEOUT_MS", "500");
        std::env::set_var("CC_STREAM_IDLE_SECS", "45");
        std::env::set_var("CC_NONSTREAM_IDLE_SECS", "120");
        std::env::set_var("CC_MAX_INFLIGHT", "8");
        std::env::set_var("CC_ACCOUNT_STRATEGY", "priority");
        std::env::set_var("CC_PREFERRED_ACCOUNT_ID", "id_env");
        let mut c = Config::default();
        c.apply_env();
        assert_eq!(c.port, 4050);
        assert_eq!(c.host, "127.0.0.1");
        assert_eq!(c.api_base, "https://env.example.com");
        assert_eq!(c.project_slug, "env-slug");
        assert_eq!(c.log_file, "/tmp/env.log");
        assert!(!c.use_provider_models);
        assert!(!c.empty_system_placeholder);
        assert!(c.zdr);
        assert_eq!(c.max_body_mb, 32);
        assert_eq!(c.client_drain_timeout_ms, 500);
        assert_eq!(c.stream_idle_timeout_secs, 45);
        assert_eq!(c.nonstream_idle_timeout_secs, 120);
        assert_eq!(c.max_inflight, 8);
        assert_eq!(c.account_strategy, "priority");
        assert_eq!(c.preferred_account_id, "id_env");

        // 非法值不生效
        std::env::set_var("PORT", "not-a-port");
        std::env::set_var("CC_MAX_BODY_MB", "0");
        std::env::set_var("CC_STREAM_IDLE_SECS", "not-a-number");
        std::env::set_var("CC_NONSTREAM_IDLE_SECS", "not-a-number");
        std::env::set_var("CC_ACCOUNT_STRATEGY", "bogus");
        std::env::set_var("CMD_ZDR", "off");
        let mut c2 = Config::default();
        c2.apply_env();
        assert_ne!(c2.port, 0);
        assert_eq!(c2.max_body_mb, 10); // 0 被拒，保持默认
        assert_eq!(c2.stream_idle_timeout_secs, 0); // 非法值被忽略，保持默认不限
        assert_eq!(c2.nonstream_idle_timeout_secs, 0);
        assert_eq!(c2.account_strategy, "round_robin");
        assert!(!c2.zdr);
        // 清理环境变量，避免影响其他测试
        for k in [
            "PORT", "HOST", "CC_API_BASE", "PROJECT_SLUG", "LOG_FILE",
            "CC_USE_PROVIDER_MODELS", "CC_EMPTY_SYSTEM_PLACEHOLDER", "CMD_ZDR",
            "CC_MAX_BODY_MB", "CC_CLIENT_DRAIN_TIMEOUT_MS", "CC_STREAM_IDLE_SECS",
            "CC_NONSTREAM_IDLE_SECS", "CC_MAX_INFLIGHT",
            "CC_ACCOUNT_STRATEGY", "CC_PREFERRED_ACCOUNT_ID",
        ] {
            std::env::remove_var(k);
        }
    }

    /// 空闲超时默认不限（0）：上游思考阶段可静默数百秒，默认设超时会误杀健康请求。
    /// 显式填 0 也必须被接受（表示不限），不能被当作非法值拒绝。
    ///
    /// 不在此处操作环境变量：apply_env 的环境变量是无作用域全局状态，与
    /// apply_env_overrides_fields 并行执行会互相干扰。
    #[test]
    fn idle_timeouts_default_to_unlimited() {
        let c = Config::default();
        assert_eq!(c.stream_idle_timeout_secs, 0);
        assert_eq!(c.nonstream_idle_timeout_secs, 0);

        // 旧配置文件缺这两个字段时应回落到默认值（serde default）
        let old: Config = serde_json::from_str(r#"{"port":3050}"#).unwrap();
        assert_eq!(old.stream_idle_timeout_secs, 0);
        assert_eq!(old.nonstream_idle_timeout_secs, 0);

        // 0 是合法值（表示不限），不能被 validate 当作非法配置拒绝
        let explicit = Config {
            stream_idle_timeout_secs: 0,
            nonstream_idle_timeout_secs: 0,
            ..Config::default()
        };
        explicit.validate().unwrap();
    }

    /// 账户对象形态缺 user_id 时按 key 派生稳定占位。
    #[test]
    fn account_object_without_user_id_derives_legacy() {
        let cfg: Config = serde_json::from_str(
            r#"{"cc_accounts":[{"key":"user_abc","user_name":"N"}]}"#,
        )
        .unwrap();
        assert!(cfg.cc_accounts[0].user_id.starts_with("legacy-"));
    }

    /// load_file：文件损坏回退默认配置；文件缺失同样回退默认。
    #[test]
    fn load_file_bad_json_or_missing_falls_back() {
        let dir = std::env::temp_dir();
        let bad = dir.join(format!("cc-config-bad-{}.json", std::process::id()));
        let _ = std::fs::remove_file(&bad);
        std::fs::write(&bad, "{ not json ").unwrap();
        let cfg = Config::load_file(&bad);
        assert_eq!(cfg.port, Config::default().port);
        let _ = std::fs::remove_file(&bad);

        let missing = dir.join(format!("cc-config-missing-{}.json", std::process::id()));
        let cfg2 = Config::load_file(&missing);
        assert_eq!(cfg2.port, Config::default().port);
    }

    /// save：父目录不存在时自动创建。
    #[test]
    fn save_creates_parent_dirs() {
        let dir = std::env::temp_dir().join(format!("cc-cfg-dir-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let path = dir.join("nested").join("config.json");
        Config::default().save(&path).unwrap();
        assert!(path.exists());
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// migrate_legacy：空白 key 不迁移。
    #[test]
    fn migrate_legacy_blank_key_skipped() {
        let mut cfg = Config::default();
        cfg.migrate_legacy(Some("   "));
        assert!(cfg.cc_accounts.is_empty());
        cfg.migrate_legacy(None);
        assert!(cfg.cc_accounts.is_empty());
    }
}
