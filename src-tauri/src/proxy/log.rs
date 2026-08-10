use serde::Serialize;
use std::collections::VecDeque;
use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};

/// 日志条目（环形缓冲，前端实时展示）。
#[derive(Debug, Clone, Serialize)]
pub struct LogEntry {
    pub seq: u64,
    pub ts: u64,
    pub level: String,
    pub msg: String,
}

pub struct LogBuffer {
    entries: Mutex<VecDeque<LogEntry>>,
    seq: Mutex<u64>,
    max: usize,
}

impl LogBuffer {
    pub const fn new(max: usize) -> Self {
        Self {
            entries: Mutex::new(VecDeque::new()),
            seq: Mutex::new(0),
            max,
        }
    }

    pub fn push(&self, level: &str, msg: &str) -> LogEntry {
        let entry = LogEntry {
            seq: {
                let mut s = self.seq.lock().unwrap();
                *s += 1;
                *s
            },
            ts: SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map(|d| d.as_millis() as u64)
                .unwrap_or(0),
            level: level.to_string(),
            msg: msg.to_string(),
        };
        let mut entries = self.entries.lock().unwrap();
        entries.push_back(entry.clone());
        while entries.len() > self.max {
            entries.pop_front();
        }
        entry
    }

    pub fn drain(&self, limit: usize, after_seq: u64) -> Vec<LogEntry> {
        let entries = self.entries.lock().unwrap();
        entries
            .iter()
            .filter(|e| e.seq > after_seq)
            .rev()
            .take(limit)
            .cloned()
            .collect::<Vec<_>>()
            .into_iter()
            .rev()
            .collect()
    }

    pub fn clear(&self) {
        self.entries.lock().unwrap().clear();
    }
}

/// 全局日志缓冲 + 事件转发器（Tauri AppHandle 在 setup 时注入）。
static LOG_BUFFER: LogBuffer = LogBuffer::new(1000);
static SINK: Mutex<Option<Box<dyn Fn(&LogEntry) + Send + Sync>>> = Mutex::new(None);

pub fn set_sink<F>(f: F)
where
    F: Fn(&LogEntry) + Send + Sync + 'static,
{
    *SINK.lock().unwrap() = Some(Box::new(f));
}

pub fn log(level: &str, msg: &str) {
    let entry = LOG_BUFFER.push(level, msg);
    if let Some(sink) = SINK.lock().unwrap().as_ref() {
        sink(&entry);
    }
}

pub fn info(msg: &str) {
    log("info", msg);
}

pub fn warn(msg: &str) {
    log("warn", msg);
}

pub fn error(msg: &str) {
    log("error", msg);
}

pub fn get_logs(limit: usize, after_seq: u64) -> Vec<LogEntry> {
    LOG_BUFFER.drain(limit, after_seq)
}

pub fn clear_logs() {
    LOG_BUFFER.clear();
}
