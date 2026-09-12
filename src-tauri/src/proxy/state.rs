use serde::Serialize;
use std::collections::{HashMap, VecDeque};
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::{Arc, Mutex, RwLock};
use std::time::{SystemTime, UNIX_EPOCH};
use tokio::sync::oneshot;

use super::config::Config;
use super::fingerprint::Fingerprint;

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

/// 对外暴露的模型条目（/v1/models 展示用）。
#[derive(Debug, Clone, Serialize)]
pub struct ModelInfo {
    /// 模型 ID（请求时使用的名称）。
    pub id: String,
    /// 模型展示名。
    pub name: String,
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
    /// 最后收到的上游 CC 事件类型（用于诊断中断位置）。
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
    /// 当前模拟的 command-code CLI 版本号。
    pub cc_version: RwLock<String>,
    /// 连续超时计数，达到阈值后在超时错误中提示缩减上下文。
    pub consecutive_timeouts: AtomicU32,
    /// 进程内在途请求计数（业务路径，/health 不计），配合 max_inflight 做并发上限。
    pub inflight: AtomicU32,
    /// 代理服务是否正在监听。
    pub running: AtomicBool,
    /// 本轮启动时间（Unix 毫秒），停止后置 None。
    pub started_at: Mutex<Option<u64>>,
    /// 最近请求队列（最新在前，上限 500 条）。
    pub requests: Mutex<VecDeque<RequestInfo>>,
    /// 复用的上游 HTTP 客户端（带 10s 连接超时）。
    pub client: reqwest::Client,
    /// 优雅停机信号发送端，serve() 的 with_graceful_shutdown 持有接收端。
    pub shutdown: Mutex<Option<oneshot::Sender<()>>>,
    /// token 用量统计数据库连接（setup 阶段初始化；代理层经此记录/查询）。
    pub usage: Mutex<Option<rusqlite::Connection>>,
    /// 指纹持久化文件路径（setup 阶段注入；None 时指纹仅存内存）。
    pub fingerprint_path: Mutex<Option<std::path::PathBuf>>,
}

impl AppState {
    /// 以给定配置构建全局状态（连接超时 10s 的 HTTP 客户端、默认 CLI 版本 0.32.3）。
    pub fn new(config: Config) -> Arc<Self> {
        Arc::new(Self {
            config: RwLock::new(config),
            sessions: Mutex::new(HashMap::new()),
            key_states: Mutex::new(HashMap::new()),
            models: RwLock::new(ModelsCache {
                models: Vec::new(),
                fetched_at: 0,
            }),
            cc_version: RwLock::new("0.32.3".into()),
            consecutive_timeouts: AtomicU32::new(0),
            inflight: AtomicU32::new(0),
            running: AtomicBool::new(false),
            started_at: Mutex::new(None),
            requests: Mutex::new(VecDeque::new()),
            client: reqwest::Client::builder()
                .connect_timeout(std::time::Duration::from_secs(10))
                .build()
                .expect("failed to build http client"),
            shutdown: Mutex::new(None),
            usage: Mutex::new(None),
            fingerprint_path: Mutex::new(None),
        })
    }

    /// 注入指纹持久化文件路径（应用启动时调用一次；不注入则指纹仅存内存）。
    pub fn set_fingerprint_path(&self, path: std::path::PathBuf) {
        *self.fingerprint_path.lock().unwrap() = Some(path);
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
                super::log::warn(&format!("记录用量失败: {e}"));
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
