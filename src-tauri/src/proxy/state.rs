use serde::{Deserialize, Serialize};
use std::collections::{HashMap, VecDeque};
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, RwLock};
use std::time::{SystemTime, UNIX_EPOCH};
use tokio::sync::oneshot;

use super::config::Config;
use super::fingerprint::Fingerprint;
use super::auth_login::AuthLoginSession;

/// 当前 Unix 毫秒时间戳（系统时钟异常时返回 0）。
pub fn now_millis() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// 当前 Unix 秒级时间戳。
pub fn now_secs() -> u64 {
    now_millis() / 1000
}

/// 尚未成功拉到上游版本号时使用的占位 CLI 版本（仅用于请求头与界面展示）。
pub const DEFAULT_CC_VERSION: &str = "0.32.3";

/// 每个 API Key 对应的模拟 CLI 会话（会话 ID 带过期时间，过期后自动重建）。
#[derive(Debug, Clone)]
pub struct SessionEntry {
    /// 模拟的 CLI 会话 ID（UUID v4）。
    pub session_id: String,
    /// 过期时间（Unix 毫秒），基础时长 12h 外加随机抖动。
    pub expires_at: u64,
}

/// 每个 API Key 的伪装状态：固定指纹与下一次初始化预请求的时间点。
#[derive(Debug, Clone)]
pub struct KeyState {
    /// 该 Key 首次使用时生成、之后保持不变的设备指纹。
    pub fingerprint: Fingerprint,
    /// 下一次执行 fingerprint/record + lifecycle 预请求的时间（Unix 毫秒），0 表示立即可做。
    pub next_init_at: u64,
}

/// 会话 → 账户的粘滞绑定（`priority` 策略用）。
///
/// 同一会话固定走同一账户，避免中途换账户导致上游 prompt 缓存失效、额度消耗变快；
/// 仅在绑定账户额度耗尽（或与会话的绑定被手动清除）时重新选择。
#[derive(Debug, Clone)]
pub struct AccountBinding {
    /// 绑定的账户 userId。
    pub user_id: String,
    /// 绑定时间（Unix 毫秒），用于超期清理。
    pub bound_at: u64,
}

/// 对外暴露的模型条目（模型列表页与 /v1/models 展示用）。
///
/// 数据来源为 `models` 表（内置列表文件播种 / Provider 端点同步），价格信息见
/// `model_pricing` 表，两者按模型 ID 关联。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelInfo {
    /// 模型 ID（请求时使用的名称，如 `tencent/hy3-paid`）。
    pub id: String,
    /// 模型展示名。
    pub name: String,
    /// 厂商显示名（由 ID 前缀推导或内置表补齐）。
    pub provider: Option<String>,
    /// 上下文长度（token）。
    pub context_length: Option<u64>,
    /// 能力标记（文本 / 视觉 / 思考）。
    pub caps: super::pricing::ModelCaps,
}

/// 模型列表缓存（避免每次 /v1/models 都请求 Provider API）。
#[derive(Debug, Clone)]
pub struct ModelsCache {
    /// 缓存的模型列表。
    pub models: Vec<ModelInfo>,
    /// 拉取时间（Unix 毫秒），0 表示尚未拉取。
    pub fetched_at: u64,
}

/// 一次代理请求的摘要（供前端中继轨道展示）。
#[derive(Debug, Clone, Serialize)]
pub struct RequestInfo {
    /// 请求 ID（同时用作下游响应体的 completion_id / message_id / response_id）。
    pub id: String,
    /// 入口路径（/v1/chat/completions 等）。
    pub path: String,
    /// 请求的模型名。
    pub model: String,
    /// 是否流式请求。
    pub stream: bool,
    pub status: String, // streaming / ok / error / timeout / disconnect
    /// 请求开始时间（Unix 毫秒）。
    pub started_at: u64,
    /// 端到端耗时（毫秒），请求结束时回填。
    pub elapsed_ms: u64,
    /// 上游回报的输入 token 数。
    pub input_tokens: u64,
    /// 上游回报的输出 token 数。
    pub output_tokens: u64,
    /// 命中缓存的输入 token 数。
    pub cached_tokens: u64,
    /// 最后收到的上游 Command Code 事件类型（用于诊断中断位置）。
    pub last_event: String,
}

/// 代理全局共享状态：配置、会话、缓存与运行控制，经 Arc 在请求与后台任务间共享。
pub struct AppState {
    /// 可热更新的运行配置。
    pub config: RwLock<Config>,
    /// api_key → 模拟 CLI 会话。
    pub sessions: Mutex<HashMap<String, SessionEntry>>,
    /// api_key → 指纹与初始化调度状态。
    pub key_states: Mutex<HashMap<String, KeyState>>,
    /// 模型列表缓存。
    pub models: RwLock<ModelsCache>,
    /// 会话（粘贴键）→ 账户 userId 的粘滞绑定（仅 `priority` 策略使用）。
    pub account_bindings: Mutex<HashMap<String, AccountBinding>>,
    /// 各账户额度快照缓存（userId → 快照 + 拉取时间），路由与额度判定共用。
    pub quota_cache: Mutex<HashMap<String, (super::quota::AccountQuota, u64)>>,
    /// 正在拉取额度的账户 userId 集合（单飞，避免并发重复请求上游）。
    pub quota_inflight: Mutex<std::collections::HashSet<String>>,
    /// 当前模拟的 command-code CLI 版本号。
    pub cc_version: RwLock<String>,
    /// 连续超时计数，达到阈值后在超时错误中提示缩减上下文。
    pub consecutive_timeouts: AtomicU32,
    /// Command Code 账户轮询游标：每个 AppState 实例独立，避免实例间（如测试）互相干扰。
    pub round_robin: AtomicUsize,
    /// 进程内在途请求计数（业务路径，/health 不计），配合 max_inflight 做并发上限。
    pub inflight: AtomicU32,
    /// 代理服务是否正在监听。
    pub running: AtomicBool,
    /// 本轮启动时间（Unix 毫秒），停止后置 None。
    pub started_at: Mutex<Option<u64>>,
    /// 最近请求队列（最新在前，上限 500 条）。
    pub requests: Mutex<VecDeque<RequestInfo>>,
    /// 复用的上游 HTTP 客户端（带 10s 连接超时；可随代理配置热更新）。
    pub client: RwLock<reqwest::Client>,
    /// 优雅停机信号发送端，serve() 的 with_graceful_shutdown 持有接收端。
    pub shutdown: Mutex<Option<oneshot::Sender<()>>>,
    /// token 用量统计数据库连接（setup 阶段初始化；代理层经此记录/查询）。
    pub usage: Mutex<Option<rusqlite::Connection>>,
    /// 浏览器授权登录会话（进行中或已完成；None 表示无进行中登录）。
    pub auth_login: Mutex<Option<AuthLoginSession>>,
    /// 进行中 loopback 回调服务器的优雅停机信号（新一轮登录时用于关停旧实例）。
    pub auth_login_shutdown: Mutex<Option<oneshot::Sender<()>>>,
    /// 最近一次启动代理的失败原因（成功启动后清空；供状态栏展示自动启动失败提示）。
    pub last_start_error: Mutex<Option<String>>,
}

impl AppState {
    /// 以给定配置构建全局状态（按代理配置构建 HTTP 客户端；CLI 版本号取配置里的本地缓存，
    /// 缓存为空时回落到内置占位版本）。client 构建失败时回退到不带代理的客户端。
    pub fn new(config: Config) -> Arc<Self> {
        let client = build_client(&config).unwrap_or_else(|_| {
            reqwest::Client::builder()
                .connect_timeout(std::time::Duration::from_secs(10))
                .build()
                .expect("failed to build http client")
        });
        // 启动时先用上次成功拉取的版本号：否则每次启动都显示内置占位版本，
        // 要等 npm 拉取成功（最长 10s 超时，失败则一直不更新）才会变成真实值
        let cc_version = if config.cc_version_cache.is_empty() {
            DEFAULT_CC_VERSION.to_string()
        } else {
            config.cc_version_cache.clone()
        };
        Arc::new(Self {
            config: RwLock::new(config),
            sessions: Mutex::new(HashMap::new()),
            key_states: Mutex::new(HashMap::new()),
            models: RwLock::new(ModelsCache {
                models: Vec::new(),
                fetched_at: 0,
            }),
            account_bindings: Mutex::new(HashMap::new()),
            quota_cache: Mutex::new(HashMap::new()),
            quota_inflight: Mutex::new(std::collections::HashSet::new()),
            cc_version: RwLock::new(cc_version),
            consecutive_timeouts: AtomicU32::new(0),
            round_robin: AtomicUsize::new(0),
            inflight: AtomicU32::new(0),
            running: AtomicBool::new(false),
            started_at: Mutex::new(None),
            requests: Mutex::new(VecDeque::new()),
            client: RwLock::new(client),
            shutdown: Mutex::new(None),
            usage: Mutex::new(None),
            auth_login: Mutex::new(None),
            auth_login_shutdown: Mutex::new(None),
            last_start_error: Mutex::new(None),
        })
    }

    /// 取当前 HTTP 客户端（读锁 + clone；reqwest::Client 内部为 Arc 共享，clone 廉价）。
    pub fn client(&self) -> reqwest::Client {
        self.client.read().unwrap().clone()
    }

    /// 按配置重建 HTTP 客户端（代理配置变更后热更新用），失败返回错误描述。
    pub fn rebuild_client(&self, config: &Config) -> Result<(), String> {
        let client = build_client(config)?;
        *self.client.write().unwrap() = client;
        Ok(())
    }

    /// 查询代理服务是否正在运行。
    pub fn is_running(&self) -> bool {
        self.running.load(Ordering::SeqCst)
    }

    /// 标记服务已启动并记录启动时间。
    pub fn mark_started(&self) {
        self.running.store(true, Ordering::SeqCst);
        *self.started_at.lock().unwrap() = Some(now_millis());
    }

    /// 标记服务已停止，并通过 oneshot 通道触发优雅停机。
    pub fn mark_stopped(&self) {
        self.running.store(false, Ordering::SeqCst);
        *self.started_at.lock().unwrap() = None;
        if let Some(tx) = self.shutdown.lock().unwrap().take() {
            let _ = tx.send(());
        }
    }

    /// 记录最近一次启动代理的失败原因（供状态栏提示；成功启动时清空）。
    pub fn set_start_error(&self, err: String) {
        *self.last_start_error.lock().unwrap() = Some(err);
    }

    /// 清空启动失败原因（启动成功后调用）。
    pub fn clear_start_error(&self) {
        *self.last_start_error.lock().unwrap() = None;
    }

    /// 记录一条请求摘要到队列头部，超出 500 条时丢弃最旧记录。
    pub fn record_request(&self, info: RequestInfo) {
        let mut q = self.requests.lock().unwrap();
        q.push_front(info);
        while q.len() > 500 {
            q.pop_back();
        }
    }

    /// 记录一条 token 用量到统计库；数据库未初始化时静默忽略（不影响代理主流程）。
    pub fn record_usage(&self, entry: &super::usage::UsageEntry) {
        let mut guard = self.usage.lock().unwrap();
        if let Some(conn) = guard.as_mut() {
            if let Err(e) = super::usage::record_usage(conn, entry) {
                super::log::warn(&format!(
                    "{}: {e}",
                    crate::i18n::pick("记录用量失败", "Failed to record usage")
                ));
            }
        }
    }

    /// 按保留天数清理超期用量数据，返回被清理的明细条数。
    pub fn prune_usage(&self, retention_days: u32) -> Result<u64, String> {
        let mut guard = self.usage.lock().unwrap();
        match guard.as_mut() {
            Some(conn) => super::usage::clear_before(conn, retention_days),
            None => Ok(0),
        }
    }

    /// 清空最近请求队列。
    pub fn clear_requests(&self) {
        self.requests.lock().unwrap().clear();
    }

    /// 取最近 `limit` 条请求摘要（最新在前）。
    pub fn recent_requests(&self, limit: usize) -> Vec<RequestInfo> {
        let q = self.requests.lock().unwrap();
        q.iter().take(limit).cloned().collect()
    }

    /// 会话基础存活时长：12 小时。
    pub fn session_duration_ms() -> u64 {
        12 * 60 * 60 * 1000
    }

    /// 会话过期时间的随机抖动上限：1 小时（打散重建时间，避免固定模式）。
    pub fn session_jitter_ms() -> u64 {
        60 * 60 * 1000
    }

    /// 初始化预请求（指纹/生命周期事件）的刷新周期：8 小时。
    pub fn init_refresh_ms() -> u64 {
        8 * 60 * 60 * 1000
    }

    /// 初始化刷新周期的随机抖动上限：2 小时。
    pub fn init_jitter_ms() -> u64 {
        2 * 60 * 60 * 1000
    }
}

/// 按出站代理配置构建 HTTP 客户端（10s 连接超时）。
///
/// - `none`：显式禁用系统代理（不读环境变量）；
/// - `system`：不设置代理，由 reqwest 的 system-proxy 特性自动读取
///   HTTP_PROXY / HTTPS_PROXY / ALL_PROXY / NO_PROXY 环境变量；
/// - `custom`：按 proxy_type 构造 socks5:// 或 http:// 代理 URL（带认证则内联用户名密码）。
pub fn build_client(config: &Config) -> Result<reqwest::Client, String> {
    let mut builder = reqwest::Client::builder()
        .connect_timeout(std::time::Duration::from_secs(10));
    match config.proxy_mode.as_str() {
        "none" => {
            builder = builder.no_proxy();
        }
        "system" => {
            // 不设置代理：reqwest 自动读系统环境变量代理
        }
        "custom" => {
            let scheme = match config.proxy_type.as_str() {
                "http" => "http",
                _ => "socks5",
            };
            let auth = if config.proxy_username.is_empty() {
                String::new()
            } else {
                format!("{}:{}@", config.proxy_username, config.proxy_password)
            };
            let url = format!("{scheme}://{auth}{}:{}", config.proxy_host, config.proxy_port);
            let proxy = reqwest::Proxy::all(&url).map_err(|e| {
                crate::i18n::err_args("proxy_build_failed", &[&e.to_string()])
            })?;
            builder = builder.proxy(proxy);
        }
        // 未知模式（validate 已拦截，防御性回退到不走代理）
        _ => {
            builder = builder.no_proxy();
        }
    }
    builder.build().map_err(|e| {
        crate::i18n::err_args("client_build_failed", &[&e.to_string()])
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// build_client 各代理模式均可构建；非法自定义代理地址返回错误。
    #[test]
    fn build_client_modes() {
        let mut c = Config::default();
        c.proxy_mode = "none".into();
        assert!(build_client(&c).is_ok());
        c.proxy_mode = "system".into();
        assert!(build_client(&c).is_ok());
        c.proxy_mode = "custom".into();
        c.proxy_type = "socks5".into();
        c.proxy_host = "127.0.0.1".into();
        c.proxy_port = 1080;
        assert!(build_client(&c).is_ok());
        // http 类型 + 认证
        c.proxy_type = "http".into();
        c.proxy_port = 3128;
        c.proxy_username = "u".into();
        c.proxy_password = "p".into();
        assert!(build_client(&c).is_ok());
        // 非法主机名 → Err
        c.proxy_host = "bad host with spaces".into();
        assert!(build_client(&c).is_err());
        // 未知模式防御性回退不走代理
        c.proxy_mode = "bogus".into();
        assert!(build_client(&c).is_ok());
    }

    /// AppState::new 遇到非法自定义代理时不 panic，回退为无代理客户端。
    #[test]
    fn new_falls_back_when_proxy_invalid() {
        let mut c = Config::default();
        c.proxy_mode = "custom".into();
        c.proxy_host = "bad host".into();
        c.proxy_port = 1;
        let st = AppState::new(c);
        assert!(!st.is_running());
    }

    /// rebuild_client 按新配置重建；失败时保留旧 client 并返回错误。
    #[test]
    fn rebuild_client_applies_new_config() {
        let st = AppState::new(Config::default());
        let mut ok = Config::default();
        ok.proxy_mode = "custom".into();
        ok.proxy_type = "http".into();
        ok.proxy_host = "127.0.0.1".into();
        ok.proxy_port = 3128;
        assert!(st.rebuild_client(&ok).is_ok());
        let mut bad = Config::default();
        bad.proxy_mode = "custom".into();
        bad.proxy_host = "bad host".into();
        bad.proxy_port = 1;
        assert!(st.rebuild_client(&bad).is_err());
    }

    /// 运行生命周期：启动/停止翻转 running，停止时发出优雅停机信号。
    #[test]
    fn running_lifecycle_and_shutdown_signal() {
        let st = AppState::new(Config::default());
        let (tx, mut rx) = tokio::sync::oneshot::channel::<()>();
        *st.shutdown.lock().unwrap() = Some(tx);
        st.mark_started();
        assert!(st.is_running());
        assert!(st.started_at.lock().unwrap().is_some());
        st.mark_stopped();
        assert!(!st.is_running());
        assert!(st.started_at.lock().unwrap().is_none());
        assert!(rx.try_recv().is_ok());
    }

    /// 请求队列：超过 500 条丢弃最旧，clear 后清空。
    #[test]
    fn request_queue_cap_and_clear() {
        let st = AppState::new(Config::default());
        for i in 0..505 {
            st.record_request(RequestInfo {
                id: format!("r{i}"),
                path: "/v1/chat/completions".into(),
                model: "m".into(),
                stream: false,
                status: "ok".into(),
                started_at: 0,
                elapsed_ms: 1,
                input_tokens: 0,
                output_tokens: 0,
                cached_tokens: 0,
                last_event: String::new(),
            });
        }
        assert_eq!(st.recent_requests(1000).len(), 500);
        assert_eq!(st.recent_requests(1)[0].id, "r504");
        st.clear_requests();
        assert!(st.recent_requests(10).is_empty());
    }

    /// 用量库未初始化时 record/prune 静默降级不报错。
    #[test]
    fn usage_without_db_degrades_silently() {
        let st = AppState::new(Config::default());
        st.record_usage(&super::super::usage::UsageEntry {
            ts: 0,
            model: "m".into(),
            endpoint: "/v1/chat/completions".into(),
            status: "ok".into(),
            prompt_tokens: 1,
            completion_tokens: 1,
            cached_tokens: 0,
            cache_write_tokens: 0,
            stream: true,
        });
        assert_eq!(st.prune_usage(30).unwrap(), 0);
    }

    /// 会话与预请求的常量配置。
    #[test]
    fn session_constants() {
        assert_eq!(AppState::session_duration_ms(), 12 * 60 * 60 * 1000);
        assert_eq!(AppState::session_jitter_ms(), 60 * 60 * 1000);
        assert_eq!(AppState::init_refresh_ms(), 8 * 60 * 60 * 1000);
        assert_eq!(AppState::init_jitter_ms(), 2 * 60 * 60 * 1000);
    }
}
