use serde::Serialize;
use std::collections::{HashMap, VecDeque};
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::{Arc, Mutex, RwLock};
use std::time::{SystemTime, UNIX_EPOCH};
use tokio::sync::oneshot;

use super::config::Config;
use super::fingerprint::Fingerprint;

pub fn now_millis() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

pub fn now_secs() -> u64 {
    now_millis() / 1000
}

#[derive(Debug, Clone)]
pub struct SessionEntry {
    pub session_id: String,
    pub expires_at: u64,
}

#[derive(Debug, Clone)]
pub struct KeyState {
    pub fingerprint: Fingerprint,
    pub next_init_at: u64,
}

#[derive(Debug, Clone, Serialize)]
pub struct ModelInfo {
    pub id: String,
    pub name: String,
}

#[derive(Debug, Clone)]
pub struct ModelsCache {
    pub models: Vec<ModelInfo>,
    pub fetched_at: u64,
}

/// 一次代理请求的摘要（供前端中继轨道展示）。
#[derive(Debug, Clone, Serialize)]
pub struct RequestInfo {
    pub id: String,
    pub path: String,
    pub model: String,
    pub stream: bool,
    pub status: String, // streaming / ok / error / timeout / disconnect
    pub started_at: u64,
    pub elapsed_ms: u64,
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub cached_tokens: u64,
    pub last_event: String,
}

pub struct AppState {
    pub config: RwLock<Config>,
    pub sessions: Mutex<HashMap<String, SessionEntry>>,
    pub key_states: Mutex<HashMap<String, KeyState>>,
    pub models: RwLock<ModelsCache>,
    pub cc_version: RwLock<String>,
    pub consecutive_timeouts: AtomicU32,
    pub running: AtomicBool,
    pub started_at: Mutex<Option<u64>>,
    pub requests: Mutex<VecDeque<RequestInfo>>,
    pub client: reqwest::Client,
    pub shutdown: Mutex<Option<oneshot::Sender<()>>>,
}

impl AppState {
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
            running: AtomicBool::new(false),
            started_at: Mutex::new(None),
            requests: Mutex::new(VecDeque::new()),
            client: reqwest::Client::builder()
                .connect_timeout(std::time::Duration::from_secs(10))
                .build()
                .expect("failed to build http client"),
            shutdown: Mutex::new(None),
        })
    }

    pub fn is_running(&self) -> bool {
        self.running.load(Ordering::SeqCst)
    }

    pub fn mark_started(&self) {
        self.running.store(true, Ordering::SeqCst);
        *self.started_at.lock().unwrap() = Some(now_millis());
    }

    pub fn mark_stopped(&self) {
        self.running.store(false, Ordering::SeqCst);
        *self.started_at.lock().unwrap() = None;
        if let Some(tx) = self.shutdown.lock().unwrap().take() {
            let _ = tx.send(());
        }
    }

    pub fn record_request(&self, info: RequestInfo) {
        let mut q = self.requests.lock().unwrap();
        q.push_front(info);
        while q.len() > 500 {
            q.pop_back();
        }
    }

    pub fn clear_requests(&self) {
        self.requests.lock().unwrap().clear();
    }

    pub fn recent_requests(&self, limit: usize) -> Vec<RequestInfo> {
        let q = self.requests.lock().unwrap();
        q.iter().take(limit).cloned().collect()
    }

    pub fn session_duration_ms() -> u64 {
        12 * 60 * 60 * 1000
    }

    pub fn session_jitter_ms() -> u64 {
        60 * 60 * 1000
    }

    pub fn init_refresh_ms() -> u64 {
        8 * 60 * 60 * 1000
    }

    pub fn init_jitter_ms() -> u64 {
        2 * 60 * 60 * 1000
    }
}
